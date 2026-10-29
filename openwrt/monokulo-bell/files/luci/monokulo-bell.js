'use strict';
'require view';
'require form';
'require rpc';
'require uci';
'require ui';

// Services > Bell: whether monokulo-bell, the Monokular Bell, runs and what
// it is doing (from its own status listener), its secrets (set or not,
// never their values) and every setting it starts with, a card per point
// of sale. Save & Apply checks the settings with the Bell's own --check
// before it restarts the Bell, and shows the Bell's words when it refuses
// them. The backend is the package's rpcd plugin (ubus object
// monokulo-bell).

var NAME = 'monokulo-bell';

var callStatus = rpc.declare({ object: NAME, method: 'status', expect: { '': {} } });
var callCheck = rpc.declare({ object: NAME, method: 'check', expect: { '': {} } });
var callSetSecret = rpc.declare({ object: NAME, method: 'set_secret', params: [ 'name', 'value' ], expect: { '': {} } });
var callClearSecret = rpc.declare({ object: NAME, method: 'clear_secret', params: [ 'name' ], expect: { '': {} } });
var callUciCommit = rpc.declare({ object: 'uci', method: 'commit', params: [ 'config' ] });
// Its own init script's actions, through its backend: the page's ACL
// grants these, not LuCI's setInitAction, which takes any service.
var callRestart = rpc.declare({ object: NAME, method: 'restart', expect: { result: false } });
var callReload = rpc.declare({ object: NAME, method: 'reload', expect: { result: false } });

// Each secret, by its variable. The VAPID key is made on install and never
// set here.
var SECRETS = [
	{ name: 'MONOKULO_BELL_VAPID_KEY', label: _('VAPID key'), generated: true,
	  help: _('Made on install. It signs every ding, and the tills’ push subscriptions are made for its public key: <strong>back it up</strong> (<code>scp root@router:/etc/monokulo-bell/secrets/MONOKULO_BELL_VAPID_KEY .</code>). Under another key every till’s subscription refuses the Bell’s dings.') },
	{ name: 'MONOKULO_BELL_SQUARE_WEBHOOK_KEY', label: _('Square webhook signature key'),
	  help: _('The signature key of the Square application’s webhook subscription, from the Developer Console’s Webhooks page. Set when, and only when, Square’s application ID is.') },
	{ name: 'MONOKULO_BELL_CLOVER_AUTH_CODE', label: _('Clover Auth Code'),
	  help: _('The Clover app’s Auth Code, from the developer dashboard’s Webhooks page. Set when, and only when, Clover’s App ID is.') }
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
	'sent': _('taken by the till’s push service'),
	'gone': _('the till’s subscription is gone: its registration was deleted'),
	'failed': _('not taken by the till’s push service')
};

// The settings saved, checked with the Bell's --check: restarted on them
// if it takes them, or left running on its earlier ones with its reason.
function checkThenReload() {
	return callCheck().then(function(res) {
		if (!res.ok) {
			ui.addNotification(_('Saved, but the Bell wasn’t restarted'), [
				E('p', {}, _('monokulo-bell refuses these settings, and keeps running on its earlier ones (if it was running). Fix them and save again:')),
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

// A secret typed (or pasted) into a dialog; the browser keeps it no longer
// than the dialog is open.
function setSecret(secret) {
	var input = E('input', { 'type': 'password', 'class': 'cbi-input-password', 'autocomplete': 'off', 'style': 'width:100%' });
	ui.showModal(_('Set %s').format(secret.label), [
		E('p', {}, secret.help),
		input,
		E('div', { 'class': 'right' }, [
			E('button', { 'class': 'btn', 'click': ui.hideModal }, [ _('Cancel') ]), ' ',
			E('button', {
				'class': 'btn cbi-button-action',
				'click': ui.createHandlerFn(this, function() {
					var value = input.value.trim();
					if (!value)
						return;
					return callSetSecret(secret.name, value).then(function(res) {
						ui.hideModal();
						if (!res.ok)
							throw new Error(res.error);
						return checkThenReload();
					}).catch(function(e) {
						showError(_('Couldn’t set %s').format(secret.label), e);
					});
				})
			}, [ _('Save') ])
		])
	]);
	input.focus();
}

function clearSecret(secret) {
	if (!window.confirm(_('Clear %s? The Bell restarts without it.').format(secret.label)))
		return;
	return callClearSecret(secret.name).then(function(res) {
		if (!res.ok)
			throw new Error(res.error);
		return checkThenReload();
	}).catch(function(e) {
		showError(_('Couldn’t clear %s').format(secret.label), e);
	});
}

// A point of sale's card: its application, environment and API address.
function pointOfSale(m, section, title, lead, id, environments) {
	var s = m.section(form.NamedSection, section, section, title, lead);
	s.addremove = false;

	var o = s.option(form.Value, 'application_id', id.label, id.help);
	o.placeholder = id.placeholder;

	o = s.option(form.ListValue, 'environment', _('Environment'), environments.help);
	o.value('', environments.byDefault);
	environments.values.forEach(function(v) { o.value(v[0], v[1]); });

	o = s.option(form.Value, 'api_url', _('API address'),
		_('Where its API is, instead of the environment’s own: for a stand-in while developing. https, or http on this router. Leave empty.'));
	o.placeholder = environments.api;
	return s;
}

return view.extend({
	load: function() {
		return Promise.all([ uci.load(NAME), L.resolveDefault(callStatus(), {}) ]);
	},

	render: function(data) {
		var st = data[1] || {};
		var own = st.status || {};
		var m, s, o;

		m = new form.Map(NAME, _('Bell'),
			_('monokulo-bell, the Monokular Bell, takes a point of sale’s payment webhooks (Square, Clover) and dings the tills registered at that location with a Web Push, so a Monokular till hears of a payment at once. It keeps the tills’ registrations alone. Save & Apply checks the settings with the Bell itself before restarting it.'));

		s = m.section(form.NamedSection, 'main', NAME, _('Status'));
		s.addremove = false;

		o = s.option(form.DummyValue, '_status', _('Bell'));
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
					? E('span', {}, [ describe(installed), ' · ' + _('installed; runs when the Bell starts') ])
					: _('Unknown: reinstall the package to see it.');
			var nodes = [ describe(running) ];
			if (installed.version && (installed.version !== running.version || installed.commit !== running.commit))
				nodes.push(E('br'), describe(installed), ' ' + _('is installed: restart the Bell to run it.'));
			return E('span', {}, nodes);
		};

		if (st.running && st.status) {
			o = s.option(form.DummyValue, '_listen', _('Listening'));
			o.rawhtml = true;
			o.cfgvalue = function() {
				return E('span', {}, [ E('code', {}, [ text(own.listen) ]), ' · ' + _('public address') + ' ',
					E('code', {}, [ text(own.public_uri) ]) ]);
			};

			o = s.option(form.DummyValue, '_stored', _('Registrations'));
			o.cfgvalue = function() {
				var stored = own.registrations && own.registrations.stored;
				return stored == null ? _('Couldn’t count them') : _('%d stored').format(stored);
			};

			o = s.option(form.DummyValue, '_last', _('Last ding'));
			o.cfgvalue = function() {
				var last = own.last_ding;
				if (!last)
					return _('None since it started');
				return ago(last.at) + ': ' + (OUTCOMES[last.outcome] || last.outcome);
			};

			o = s.option(form.DummyValue, '_ready', _('Points of sale'));
			o.cfgvalue = function() {
				var p = own.points_of_sale || {};
				var say = function(state) { return state === 'ready' ? _('on') : _('off'); };
				return _('Square %s · Clover %s').format(say(p.square), say(p.clover));
			};
		}

		o = s.option(form.DummyValue, '_secrets', _('Secrets'),
			_('Kept in <code>/etc/monokulo-bell/secrets/</code>, root’s alone, one file per secret. This page never shows them.'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			var set = st.secrets || {};
			return E('div', {}, SECRETS.map(function(secret) {
				var buttons = [];
				if (!secret.generated) {
					buttons.push(E('button', {
						'class': 'cbi-button cbi-button-action',
						'click': ui.createHandlerFn(this, setSecret, secret)
					}, [ set[secret.name] ? _('Replace…') : _('Set…') ]));
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
			_('Stops the Bell and starts it again with the settings saved here.'));
		o.inputtitle = _('Restart the Bell');
		o.inputstyle = 'reload';
		o.onclick = function() {
			return callRestart().then(function() {
				window.location.reload();
			}).catch(function(e) {
				showError(_('Couldn’t restart the Bell'), e);
			});
		};

		o = s.option(form.DummyValue, '_tls', _('https'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return _('The Bell serves plain HTTP on this router only (127.0.0.1) by default: the points of sale and the tills reach it at its https public address, through a TLS proxy here. Caddy is one package (<code>apk add caddy</code>) with a three-line <code>/etc/caddy/Caddyfile</code>: <code>bell.example.org {</code> <code>reverse_proxy 127.0.0.1:8445</code> <code>}</code>, with its certificate from Let’s Encrypt (ports 80 and 443 open to the internet). More, nginx included: <a href="https://docs.monokulo.com/services/" target="_blank" rel="noreferrer">docs.monokulo.com/services</a>.');
		};

		// Server
		s = m.section(form.NamedSection, 'main', NAME, _('Server'));
		s.addremove = false;

		o = s.option(form.Flag, 'enabled', _('Enabled'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Value, 'public_uri', _('Public address'),
			_('The https address the points of sale and the tills reach this Bell at, such as https://bell.example.org. Each point of sale’s webhooks go to it followed by /v1/square/webhook or /v1/clover/webhook; Square’s subscription must name that URL exactly. Required.'));
		o.placeholder = 'https://bell.example.org';

		o = s.option(form.Value, 'port', _('Port'),
			_('The port the Bell listens on. To reach it from the internet without a proxy on this router, turn on the rule <em>Allow-monokulo-bell</em> under Network › Firewall › Traffic Rules (made off on install); behind a proxy here, open the proxy’s 443 instead.'));
		o.datatype = 'port';
		o.placeholder = '8445';
		o.rmempty = false;

		o = s.option(form.ListValue, 'listen', _('Listen on'));
		o.value('loopback', _('This router only (127.0.0.1): a TLS proxy here in front'));
		o.value('lan', _('LAN only'));
		o.value('all', _('All interfaces'));
		o.default = 'loopback';

		o = s.option(form.DynamicList, 'trusted_proxy', _('Trusted proxies'),
			_('Addresses or CIDR ranges of the reverse proxies in front of the Bell. A registration from one of them is counted against the address its X-Forwarded-For names, not the proxy’s. This router (127.0.0.1, ::1) when none are given.'));
		o.placeholder = '127.0.0.1';

		o = s.option(form.Value, 'data_dir', _('Data folder'),
			_('The Bell’s database (bell.db: the tills’ registrations alone). Must be on storage, not /tmp. Kept across firmware upgrades if left at the default.'));
		o.placeholder = '/srv/monokulo-bell';
		o.validate = function(section_id, value) {
			if (!value)
				return true;
			if (!/^\/[^\s]*$/.test(value))
				return _('Use an absolute path such as /srv/monokulo-bell.');
			if (/^\/(tmp|var)(\/|$)/.test(value))
				return _('/tmp and /var are in memory: everything there is lost on reboot.');
			return true;
		};

		o = s.option(form.Value, 'timeout_ms', _('Timeout (ms)'),
			_('Milliseconds the Bell waits for a point of sale (checking a registration’s location) or a push service (taking a ding) to answer.'));
		o.datatype = 'range(100,120000)';
		o.placeholder = '10000';

		o = s.option(form.Value, 'registrations_per_minute', _('Registrations a minute per address'),
			_('Registrations a minute from any one address (IPv6 by its /64). Each costs a request to the till’s point of sale.'));
		o.datatype = 'range(1,1000000)';
		o.placeholder = '30';

		o = s.option(form.Value, 'registration_failure_limit', _('Failed dings before a registration goes'),
			_('Dings in a row a till’s push service may fail to take before its registration is deleted. One it says is gone is deleted at once.'));
		o.datatype = 'range(1,1000)';
		o.placeholder = '20';

		o = s.option(form.Value, 'dings_per_merchant_per_minute', _('Dings a minute per merchant'),
			_('Dings one merchant’s tills are sent a minute, across all its locations; the rest of the minute’s are dropped. Keep it well below the push relay’s per-sender limit (600 a minute by default): every merchant’s dings are signed with the Bell’s one key.'));
		o.datatype = 'range(1,1000000)';
		o.placeholder = '120';

		o = s.option(form.Flag, 'allow_private_endpoints', _('Private push endpoints'),
			_('Let a till register a push endpoint on a loopback, private or link-local address, such as a push service on this network. Off, every ding goes to a public address only. Never an onion address.'));
		o.default = '0';

		// The points of sale
		pointOfSale(m, 'square', _('Square'),
			_('Set the application ID to serve Square tills, and its webhook signature key under Secrets.'),
			{ label: _('Application ID'), placeholder: 'sq0idp-…',
			  help: _('The Square application whose webhooks this Bell receives, and which the tills sign in through: sq0idp-… in production, sandbox-sq0idb-… in the sandbox.') },
			{ help: _('Which Square the application is in.'), byDefault: _('Production'),
			  values: [ [ 'sandbox', _('Sandbox, for testing') ] ], api: 'https://connect.squareup.com' });

		pointOfSale(m, 'clover', _('Clover'),
			_('Set the App ID to serve Clover tills, and its Auth Code under Secrets.'),
			{ label: _('App ID'), placeholder: 'ABCDEFGHJKMNP',
			  help: _('The Clover app whose webhooks this Bell receives, and which the tills sign in through: its App ID, 13 letters and digits, from the developer dashboard of the environment below.') },
			{ help: _('Which Clover the app is in: the region its merchants are in, or the sandbox. One Bell serves one.'), byDefault: _('North America'),
			  values: [ [ 'europe', _('Europe') ], [ 'latin_america', _('Latin America') ], [ 'sandbox', _('Sandbox, for testing') ] ],
			  api: 'https://api.clover.com' });

		// Logging
		s = m.section(form.NamedSection, 'main', NAME, _('Logging'),
			_('The Bell’s log lines go to the system log (Status › System Log). No line carries a webhook, a point of sale’s token or a push endpoint; the one exception is Clover’s webhook verification code, logged at info for you to enter in Clover’s dashboard.'));
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
	// Bell takes them.
	handleSaveApply: function(ev) {
		return this.handleSave(ev).then(function() {
			return callUciCommit(NAME);
		}).then(checkThenReload).catch(function(e) {
			showError(_('Couldn’t save'), e);
		});
	}
});
