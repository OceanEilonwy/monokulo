// The backend of a service's LuCI page, over ubus: an rpcd ucode plugin
// that monokulo-push-relay and monokulo-bell each install as
// /usr/share/rpcd/ucode/<package>, exposing the object <package> (named by
// the file it is installed as). monokulo installs it too, for `status`
// alone (its settings are on its own admin page):
//   status        whether it runs, its build, why it didn't start, which
//                 secrets are set (never their values) and the service's own
//                 GET /status from its loopback status listener
//   check         the settings and secrets as the next start would take
//                 them, through the binary's --check (the init script's
//                 `check`)
//   set_secret    {name, value}: a secret's file in /etc/<package>/secrets/
//   clear_secret  {name}
// LuCI can't read the root-only secrets folder, nor run a command, without
// this; its ACL grants the page these methods alone. Nor need it procd's
// `service list`, whose instances hold every secret in their environment:
// `status` takes what the page shows from it here.

'use strict';

import { readfile, writefile, rename, unlink, chmod, access, popen, basename } from 'fs';
import { connect } from 'ubus';

// Each service's secrets, by their variables (the one made on install
// first), and its loopback status listener.
const SERVICES = {
	'monokulo': {},
	'monokulo-push-relay': {
		secrets: [ 'MONOKULO_RELAY_HANDLE_KEY', 'MONOKULO_RELAY_APNS_KEY', 'MONOKULO_RELAY_FCM_CREDENTIALS' ],
		generated: 'MONOKULO_RELAY_HANDLE_KEY',
		status_url: 'http://127.0.0.1:8446/status'
	},
	'monokulo-bell': {
		secrets: [ 'MONOKULO_BELL_VAPID_KEY', 'MONOKULO_BELL_SQUARE_WEBHOOK_KEY', 'MONOKULO_BELL_CLOVER_AUTH_CODE' ],
		generated: 'MONOKULO_BELL_VAPID_KEY',
		status_url: 'http://127.0.0.1:8447/status'
	}
};

const name = basename(sourcepath());
const service = SERVICES[name];
// Whether the page sets the service's secrets and checks its settings.
const configured = service?.secrets != null;
const secrets_dir = `/etc/${name}/secrets`;
// The largest secret taken: an APNs key is about 250 bytes, a Firebase
// service account about 2.4 KB.
const MAX_SECRET = 65536;

// The output and exit code of a command run by the shell, stderr included.
function run(command) {
	const proc = popen(`${command} 2>&1`);
	if (!proc)
		return { output: '', code: -1 };
	const output = proc.read('all') ?? '';
	return { output: trim(output), code: proc.close() };
}

// version= and commit= lines, as the package's version file holds them.
function build(text) {
	const found = {};
	for (let line in split(text ?? '', '\n')) {
		const kv = match(line, /^(version|commit)=(.*)$/);
		if (kv)
			found[kv[1]] = trim(kv[2]);
	}
	return found;
}

function first_line_file(path) {
	const text = readfile(path);
	return text == null ? null : trim(text);
}

// The service's own status, from its status listener on this router.
function own_status() {
	const fetched = run(`uclient-fetch -q -T 3 -O - '${service.status_url}'`);
	if (fetched.code != 0)
		return null;
	try {
		return json(fetched.output);
	}
	catch (e) {
		return null;
	}
}

function known_secret(secret) {
	return type(secret) == 'string' && index(service.secrets, secret) >= 0;
}

const methods = {
	status: {
		call: function() {
			const ubus = connect();
			const listed = ubus?.call('service', 'list', { name });
			const instance = listed?.[name]?.instances?.[name];
			const running = instance?.running == true;
			const secrets = {};
			for (let secret in service.secrets ?? [])
				secrets[secret] = length(readfile(`${secrets_dir}/${secret}`) ?? '') > 0;
			return {
				running,
				pid: running ? instance.pid : null,
				running_build: running ? { version: instance.data?.version, commit: instance.data?.commit } : null,
				installed: build(readfile(`/usr/share/${name}/version`)),
				check_error: first_line_file(`/tmp/${name}.check`),
				blocked_by: first_line_file(`/tmp/${name}.blocked`),
				secrets,
				generated_secret: service.generated,
				status: running && service.status_url ? own_status() : null
			};
		}
	},

	check: {
		call: function() {
			const checked = run(`/etc/init.d/${name} check`);
			return { ok: checked.code == 0, output: checked.output };
		}
	},

	set_secret: {
		args: { name: 'name', value: 'value' },
		call: function(request) {
			const secret = request.args.name, value = request.args.value;
			if (!known_secret(secret) || secret == service.generated)
				return { ok: false, error: 'Not a secret LuCI sets.' };
			if (type(value) != 'string' || length(trim(value)) == 0)
				return { ok: false, error: 'The value is empty: clear the secret instead.' };
			if (length(value) > MAX_SECRET || index(value, '\0') >= 0)
				return { ok: false, error: 'That is not a secret this service takes.' };
			const path = `${secrets_dir}/${secret}`;
			// The folder is root's alone (the init script makes it), so the
			// file is never readable by anyone else, even before its chmod.
			if (!access(secrets_dir) && run(`/etc/init.d/${name} secrets`).code != 0)
				return { ok: false, error: `Couldn't make ${secrets_dir}.` };
			if (writefile(`${path}.new`, value) == null || !chmod(`${path}.new`, 0o600) || !rename(`${path}.new`, path)) {
				unlink(`${path}.new`);
				return { ok: false, error: `Couldn't write ${path}.` };
			}
			return { ok: true };
		}
	},

	clear_secret: {
		args: { name: 'name' },
		call: function(request) {
			const secret = request.args.name;
			if (!known_secret(secret) || secret == service.generated)
				return { ok: false, error: 'Not a secret LuCI clears.' };
			const path = `${secrets_dir}/${secret}`;
			if (access(path) && !unlink(path))
				return { ok: false, error: `Couldn't remove ${path}.` };
			return { ok: true };
		}
	}
};

if (!service)
	return {};
return { [name]: configured ? methods : { status: methods.status } };
