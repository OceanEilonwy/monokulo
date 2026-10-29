'use strict';
'require view';
'require form';
'require rpc';
'require uci';
'require ui';

// Services > Push relay: whether monokulo-push-relay runs and what it is
// doing (from its own status listener), its secrets (set or not, never
// their values) and every setting it starts with. Save & Apply checks the
// settings with the relay's own --check before it restarts the relay, and
// shows the relay's words when it refuses them. The backend is the
// package's rpcd plugin (ubus object monokulo-push-relay).

var NAME = 'monokulo-push-relay';

var callStatus = rpc.declare({ object: NAME, method: 'status', expect: { '': {} } });
var callCheck = rpc.declare({ object: NAME, method: 'check', expect: { '': {} } });
var callSetSecret = rpc.declare({ object: NAME, method: 'set_secret', params: [ 'name', 'value' ], expect: { '': {} } });
var callClearSecret = rpc.declare({ object: NAME, method: 'clear_secret', params: [ 'name' ], expect: { '': {} } });
var callUciCommit = rpc.declare({ object: 'uci', method: 'commit', params: [ 'config' ] });
// Its own init script's actions, through its backend: the page's ACL
// grants these, not LuCI's setInitAction, which takes any service.
var callRestart = rpc.declare({ object: NAME, method: 'restart', expect: { result: false } });
var callReload = rpc.declare({ object: NAME, method: 'reload', expect: { result: false } });

// Each secret, by its variable: what it is, and whether it is a file to
// upload. The handle key is made on install and never set here.
var SECRETS = [
	{ name: 'MONOKULO_RELAY_HANDLE_KEY', label: _('Handle key'), generated: true,
	  help: _('Made on install. Every endpoint the relay hands out is made under it: <strong>back it up</strong> (<code>scp root@router:/etc/monokulo-push-relay/secrets/MONOKULO_RELAY_HANDLE_KEY .</code>), apart from the data folder. Under another key every device’s next registration gets a new endpoint.') },
	{ name: 'MONOKULO_RELAY_APNS_KEY', label: _('APNs key'), file: true,
	  help: _('The AuthKey_&lt;id&gt;.p8 file Apple gave you, for iPhones. Needs the Apple team ID and key ID below.') },
	{ name: 'MONOKULO_RELAY_FCM_CREDENTIALS', label: _('FCM service account'), file: true,
	  help: _('The Firebase service account’s JSON key file, for Android.') }
];

// Where a build's commit link points.
var REPOSITORY = 'https://github.com/OceanEilonwy/monokulo';

function text(value) {
	return document.createTextNode(value == null ? '' : String(value));
}

// A build to read: the version, and the start of its commit linked to it.
function describe(b) {
	var nodes = [ E('code', {}, [ text(b.version) ]) ];
	if (/^[0-9a-f]{40}$/.test(b.commit || ''))
		nodes.push(' · ' + _('commit') + ' ', E('a', {
			href: REPOSITORY + '/commit/' + b.commit, target: '_blank', rel: 'noreferrer'
		}, [ E('code', {}, [ text(b.commit.substring(0, 12)) ]) ]));
	return E('span', {}, nodes);
}

// When something happened (Unix seconds), as how long ago.
function ago(at) {
	var seconds = Math.max(0, Math.floor(Date.now() / 1000) - at);
	return _('%s ago').format('%t'.format(seconds));
}

var OUTCOMES = {
	'sent': _('taken by APNs or FCM'),
	'gone': _('the device is gone: its handle was forgotten'),
	'refused': _('refused, for a reason of this relay’s (its keys or settings)'),
	'unavailable': _('APNs or FCM was down or busy'),
	'too-large': _('too large for the device')
};

// The settings saved, checked with the relay's --check: restarted on them
// if it takes them, or left running on its earlier ones with its reason.
function checkThenReload() {
	return callCheck().then(function(res) {
		if (!res.ok) {
			ui.addNotification(_('Saved, but the relay wasn’t restarted'), [
				E('p', {}, _('monokulo-push-relay refuses these settings, and keeps running on its earlier ones (if it was running). Fix them and save again:')),
				E('pre', {}, [ text(res.output) ])
			], 'danger');
			return false;
		}
		return callReload().then(function() {
			window.setTimeout(function() { window.location.reload(); }, 1500);
			return true;
		});
	});
}

function showError(title, e) {
	ui.addNotification(null, E('p', {}, [ text(title + ': ' + (e.message || e)) ]), 'danger');
}

function setSecret(secret, value) {
	return callSetSecret(secret.name, value).then(function(res) {
		if (!res.ok)
			throw new Error(res.error);
		return checkThenReload();
	}).catch(function(e) {
		showError(_('Couldn’t set %s').format(secret.label), e);
	});
}

// A file's text, chosen in the browser, as the secret.
function uploadSecret(secret) {
	var input = E('input', { type: 'file', style: 'display:none' });
	input.addEventListener('change', function() {
		var file = input.files[0];
		if (!file)
			return;
		var reader = new FileReader();
		reader.onload = function() { setSecret(secret, reader.result); };
		reader.onerror = function() { showError(_('Couldn’t read %s').format(file.name), reader.error); };
		reader.readAsText(file);
	});
	document.body.appendChild(input);
	input.click();
	input.remove();
}

function clearSecret(secret) {
	if (!window.confirm(_('Clear %s? The relay restarts without it.').format(secret.label)))
		return;
	return callClearSecret(secret.name).then(function(res) {
		if (!res.ok)
			throw new Error(res.error);
		return checkThenReload();
	}).catch(function(e) {
		showError(_('Couldn’t clear %s').format(secret.label), e);
	});
}

return view.extend({
	load: function() {
		return Promise.all([ uci.load(NAME), L.resolveDefault(callStatus(), {}) ]);
	},

	render: function(data) {
		var st = data[1] || {};
		var own = st.status || {};
		var m, s, o;

		m = new form.Map(NAME, _('Push relay'),
			_('monokulo-push-relay hands the Web Push monokulo and the Monokular Bell send to a till app on to Apple’s (APNs) or Google’s (FCM) push service, under the app’s own keys. It keeps its handles alone, never a push. Save & Apply checks the settings with the relay itself before restarting it.'));

		s = m.section(form.NamedSection, 'main', NAME, _('Status'));
		s.addremove = false;

		o = s.option(form.DummyValue, '_status', _('Push relay'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			var nodes = [ E('strong', {}, [ st.running ? _('Running') : _('Not running') ]) ];
			if (st.blocked_by)
				nodes.push(E('p', {}, [ text(st.blocked_by) ]));
			else if (st.check_error)
				nodes.push(E('p', {}, st.running
					? _('It runs on its earlier settings: it refused the ones saved last.')
					: _('It didn’t start:')),
					E('pre', {}, [ text(st.check_error) ]));
			else if (!st.running && uci.get(NAME, 'main', 'enabled') === '0')
				nodes.push(' · ' + _('turned off below'));
			return E('div', {}, nodes);
		};

		o = s.option(form.DummyValue, '_version', _('Version'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			var running = st.running_build || {}, installed = st.installed || {};
			if (!running.version)
				return installed.version
					? E('span', {}, [ describe(installed), ' · ' + _('installed; runs when the relay starts') ])
					: _('Unknown: reinstall the package to see it.');
			var nodes = [ describe(running) ];
			if (installed.version && (installed.version !== running.version || installed.commit !== running.commit))
				nodes.push(E('br'), describe(installed), ' ' + _('is installed: restart the relay to run it.'));
			return E('span', {}, nodes);
		};

		if (st.running && st.status) {
			o = s.option(form.DummyValue, '_listen', _('Listening'));
			o.rawhtml = true;
			o.cfgvalue = function() {
				return E('span', {}, [ E('code', {}, [ text(own.listen) ]), ' · ' + _('public address') + ' ',
					E('code', {}, [ text(own.public_uri) ]) ]);
			};

			o = s.option(form.DummyValue, '_stored', _('Handles'));
			o.cfgvalue = function() {
				var stored = own.handles && own.handles.stored;
				var apps = (own.apps || []).join(', ') || _('none');
				return (stored == null ? _('Couldn’t count them') : _('%d stored').format(stored)) + ' · ' + _('apps: %s').format(apps);
			};

			o = s.option(form.DummyValue, '_last', _('Last push'));
			o.cfgvalue = function() {
				var last = own.last_delivery;
				if (!last)
					return _('None since it started');
				return ago(last.at) + ': ' + (OUTCOMES[last.outcome] || last.outcome);
			};

			o = s.option(form.DummyValue, '_ready', _('Ready'));
			o.cfgvalue = function() {
				var p = own.platforms || {};
				var say = function(state) { return state === 'ready' ? _('ready') : _('not configured'); };
				return _('APNs (iPhone) %s · FCM (Android) %s').format(say(p.apns), say(p.fcm));
			};
		}

		o = s.option(form.DummyValue, '_secrets', _('Secrets'),
			_('Kept in <code>/etc/monokulo-push-relay/secrets/</code>, root’s alone, one file per secret. This page never shows them.'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			var set = st.secrets || {};
			return E('div', {}, SECRETS.map(function(secret) {
				var buttons = [];
				if (!secret.generated) {
					buttons.push(E('button', {
						'class': 'cbi-button cbi-button-action',
						'click': ui.createHandlerFn(this, uploadSecret, secret)
					}, [ set[secret.name] ? _('Replace…') : _('Upload…') ]));
					if (set[secret.name])
						buttons.push(' ', E('button', {
							'class': 'cbi-button cbi-button-remove',
							'click': ui.createHandlerFn(this, clearSecret, secret)
						}, [ _('Clear') ]));
				}
				return E('div', { 'style': 'margin-bottom:1em' }, [
					E('strong', {}, [ secret.label ]), ' ',
					E('code', {}, [ secret.name ]), ': ',
					set[secret.name] ? _('set') : _('not set'), ' ',
					E('span', {}, buttons),
					E('div', { 'class': 'cbi-value-description' }, secret.help)
				]);
			}));
		};

		o = s.option(form.Button, '_restart', _('Restart'),
			_('Stops the relay and starts it again with the settings saved here.'));
		o.inputtitle = _('Restart the relay');
		o.inputstyle = 'reload';
		o.onclick = function() {
			return callRestart().then(function() {
				window.location.reload();
			}).catch(function(e) {
				showError(_('Couldn’t restart the relay'), e);
			});
		};

		o = s.option(form.DummyValue, '_tls', _('https'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return _('The relay serves plain HTTP: apps and senders reach it at its https public address, through a TLS proxy. On this router, Caddy is one package (<code>apk add caddy</code>) with a three-line <code>/etc/caddy/Caddyfile</code>: <code>relay.example.org {</code> <code>reverse_proxy 127.0.0.1:8444</code> <code>}</code>, with its certificate from Let’s Encrypt (ports 80 and 443 open to the internet). With the proxy here, set “Listen on” to “This router only” and 127.0.0.1 among the trusted proxies. More, nginx included: <a href="https://docs.monokulo.com/services/" target="_blank" rel="noreferrer">docs.monokulo.com/services</a>.');
		};

		// Server
		s = m.section(form.NamedSection, 'main', NAME, _('Server'));
		s.addremove = false;

		o = s.option(form.Flag, 'enabled', _('Enabled'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Value, 'public_uri', _('Public address'),
			_('The https address apps and senders reach this relay at, such as https://relay.example.org: every endpoint it hands out is under it, and a push is accepted only when its VAPID token is signed for it. Required. An origin alone (no path).'));
		o.placeholder = 'https://relay.example.org';

		o = s.option(form.Value, 'port', _('Port'),
			_('The port the relay listens on. To reach it from the internet without a proxy on this router, turn on the rule <em>Allow-monokulo-push-relay</em> under Network › Firewall › Traffic Rules (made off on install); behind a proxy here, open the proxy’s 443 instead.'));
		o.datatype = 'port';
		o.placeholder = '8444';
		o.rmempty = false;

		o = s.option(form.ListValue, 'listen', _('Listen on'));
		o.value('lan', _('LAN only'));
		o.value('all', _('All interfaces'));
		o.value('loopback', _('This router only (127.0.0.1): a TLS proxy here in front'));
		o.default = 'lan';

		o = s.option(form.DynamicList, 'trusted_proxy', _('Trusted proxies'),
			_('Addresses or CIDR ranges of the reverse proxies in front of the relay. A request from one of them is counted against the address its X-Forwarded-For names, not the proxy’s. Empty when nothing is in front.'));
		o.placeholder = '127.0.0.1';

		o = s.option(form.Value, 'upstream_timeout_ms', _('Upstream timeout (ms)'),
			_('Milliseconds each push gets to reach APNs or FCM before the sender is answered 503, to try again later. Keep it below the senders’ own timeouts.'));
		o.datatype = 'range(100,120000)';
		o.placeholder = '4000';

		o = s.option(form.Value, 'data_dir', _('Data folder'),
			_('The relay’s database (push-relay.db: its handles alone). Must be on storage, not /tmp. Kept across firmware upgrades if left at the default.'));
		o.placeholder = '/srv/monokulo-push-relay';
		o.validate = function(section_id, value) {
			if (!value)
				return true;
			if (!/^\/[^\s]*$/.test(value))
				return _('Use an absolute path such as /srv/monokulo-push-relay.');
			if (/^\/(tmp|var)(\/|$)/.test(value))
				return _('/tmp and /var are in memory: everything there is lost on reboot.');
			return true;
		};

		o = s.option(form.Value, 'ttl_days', _('Handle lifetime (days)'),
			_('Days a handle lasts after its last registration or the last push APNs or FCM took for it. After that it is forgotten and its endpoint answers 404.'));
		o.datatype = 'range(1,3650)';
		o.placeholder = '90';

		o = s.option(form.Value, 'unused_ttl_days', _('Unused handle lifetime (days)'),
			_('Days a handle no push has reached yet lasts after its registration: at most the handle lifetime.'));
		o.datatype = 'range(1,3650)';
		o.placeholder = '7';

		// Apps
		s = m.section(form.GridSection, 'app', _('Apps'),
			_('The apps devices may register for, by the name they register with (lowercase letters, digits and hyphens), each with its iOS bundle ID for APNs and its Firebase project ID for FCM, either or both.'));
		s.anonymous = true;
		s.addremove = true;
		s.nodescriptions = true;

		o = s.option(form.Value, 'name', _('Name'));
		o.placeholder = 'monokular';
		o.rmempty = false;
		o.validate = function(section_id, value) {
			return /^[a-z0-9-]+$/.test(value || '') ? true : _('Lowercase letters, digits and hyphens.');
		};

		o = s.option(form.Value, 'apns_topic', _('iOS bundle ID (APNs)'));
		o.placeholder = 'com.example.monokular';

		o = s.option(form.Value, 'fcm_project_id', _('Firebase project ID (FCM)'));
		o.placeholder = 'monokular-12345';

		// Limits
		s = m.section(form.NamedSection, 'main', NAME, _('Limits'));
		s.addremove = false;

		o = s.option(form.Value, 'per_sender_per_minute', _('Pushes a minute per sender'),
			_('Pushes a minute each sender (a VAPID key: a whole monokulo, or a whole Bell) may hand on, to all its handles together. Size it for the busiest sender you serve.'));
		o.datatype = 'range(1,1000000)';
		o.placeholder = '600';

		o = s.option(form.Value, 'per_handle_per_minute', _('Pushes a minute per handle'),
			_('Pushes a minute to any one handle. A sale is at most five pushes, so this only stops a sender gone wrong from draining a device’s battery.'));
		o.datatype = 'range(1,1000000)';
		o.placeholder = '60';

		o = s.option(form.Value, 'registrations_per_minute', _('Registrations a minute per address'),
			_('Registrations a minute from any one address (IPv6 by its /64).'));
		o.datatype = 'range(1,1000000)';
		o.placeholder = '30';

		// Apple
		s = m.section(form.NamedSection, 'main', NAME, _('Apple (APNs)'),
			_('For iPhones: the team ID and key ID of the APNs key uploaded under Secrets.'));
		s.addremove = false;

		o = s.option(form.Value, 'apns_team_id', _('Team ID'),
			_('The Apple Developer team ID the APNs key belongs to: 10 letters and digits, from the membership page.'));
		o.placeholder = 'ABCDE12345';

		o = s.option(form.Value, 'apns_key_id', _('Key ID'),
			_('The APNs key’s ID: 10 letters and digits, the &lt;id&gt; of its AuthKey_&lt;id&gt;.p8.'));
		o.placeholder = 'FGHIJ67890';

		o = s.option(form.ListValue, 'apns_endpoint', _('APNs'),
			_('Where APNs is: production, or the sandbox for apps built for development.'));
		o.value('', _('Production (https://api.push.apple.com)'));
		o.value('https://api.sandbox.push.apple.com', _('Sandbox (https://api.sandbox.push.apple.com)'));

		// Google
		s = m.section(form.NamedSection, 'main', NAME, _('Google (FCM)'),
			_('For Android: the service account uploaded under Secrets.'));
		s.addremove = false;

		o = s.option(form.Value, 'fcm_endpoint', _('FCM endpoint'),
			_('Where Firebase Cloud Messaging’s HTTP v1 API is.'));
		o.placeholder = 'https://fcm.googleapis.com';

		// Logging
		s = m.section(form.NamedSection, 'main', NAME, _('Logging'),
			_('The relay’s log lines go to the system log (Status › System Log). No line carries a token, an endpoint or a push.'));
		s.addremove = false;

		o = s.option(form.Value, 'log_level', _('Level'),
			_('A level (error, warn, info, debug, trace), optionally followed by target=level pairs.'));
		o.placeholder = 'info';

		o = s.option(form.ListValue, 'log_format', _('Format'));
		o.value('', _('Automatic (JSON here)'));
		o.value('json', _('JSON'));
		o.value('pretty', _('Pretty'));

		return m.render();
	},

	// Save, commit, then --check: restart on the new settings only if the
	// relay takes them.
	handleSaveApply: function(ev) {
		return this.handleSave(ev).then(function() {
			return callUciCommit(NAME);
		}).then(checkThenReload).catch(function(e) {
			showError(_('Couldn’t save'), e);
		});
	}
});
