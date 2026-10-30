'use strict';
'require view';
'require form';
'require rpc';
'require uci';
'require ui';

// Services > Bell: whether monokulo-bell, the Monokular Bell, runs and what
// it is doing (from its own status listener), its secrets (set or not,
// never their values) and every setting it starts with, a card per
// integration (a point of sale's environment), the ones beyond production
// behind a disclosure. Save & Apply checks the settings with the Bell's own --check
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

// The VAPID key, made on install and never set here.
var VAPID = { name: 'MONOKULO_BELL_VAPID_KEY', label: _('VAPID key'), generated: true,
	help: _('Made on install. It signs every ding, and the tills’ push subscriptions are made for its public key: <strong>back it up</strong> (<code>scp root@router:/etc/monokulo-bell/secrets/MONOKULO_BELL_VAPID_KEY .</code>). Under another key every till’s subscription refuses the Bell’s dings.') };

// Each integration (a point of sale's environment): its UCI section, named
// <point of sale>_<environment>, its secret by its variable, and where its
// API is unless set.
var SQUARE = {
	title: _('Square'), id: _('Application ID'),
	idHelp: _('The application whose webhooks this Bell receives in this environment, and which the tills sign in through, from the Developer Console.'),
	secret: _('webhook signature key'),
	secretHelp: _('The signature key of this application’s webhook subscription, from the Developer Console’s Webhooks page. Set when, and only when, the application ID is.')
};
var CLOVER = {
	title: _('Clover'), id: _('App ID'),
	idHelp: _('The app whose webhooks this Bell receives in this environment, and which the tills sign in through: its App ID, 13 letters and digits, from this environment’s developer dashboard.'),
	secret: _('Auth Code'),
	secretHelp: _('This app’s Auth Code, from the developer dashboard’s Webhooks page. Set when, and only when, the App ID is.')
};
var INTEGRATIONS = {
	square_production: { pos: SQUARE, wire: [ 'square', 'production' ], name: _('production'),
		placeholder: 'sq0idp-…', api: 'https://connect.squareup.com',
		secret: 'MONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY' },
	square_sandbox: { pos: SQUARE, wire: [ 'square', 'sandbox' ], name: _('sandbox'),
		placeholder: 'sandbox-sq0idb-…', api: 'https://connect.squareupsandbox.com',
		secret: 'MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY' },
	clover_north_america: { pos: CLOVER, wire: [ 'clover', 'north_america' ], name: _('North America'),
		placeholder: 'ABCDEFGHJKMNP', api: 'https://api.clover.com',
		secret: 'MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE' },
	clover_europe: { pos: CLOVER, wire: [ 'clover', 'europe' ], name: _('Europe'),
		placeholder: 'ABCDEFGHJKMNP', api: 'https://api.eu.clover.com',
		secret: 'MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE' },
	clover_latin_america: { pos: CLOVER, wire: [ 'clover', 'latin_america' ], name: _('Latin America'),
		placeholder: 'ABCDEFGHJKMNP', api: 'https://api.la.clover.com',
		secret: 'MONOKULO_BELL_CLOVER_LATIN_AMERICA_AUTH_CODE' },
	clover_sandbox: { pos: CLOVER, wire: [ 'clover', 'sandbox' ], name: _('sandbox'),
		placeholder: 'ABCDEFGHJKMNP', api: 'https://apisandbox.dev.clover.com',
		secret: 'MONOKULO_BELL_CLOVER_SANDBOX_AUTH_CODE' }
};

// How the integrations' cards are grouped: most operators use production
// alone, so the others are behind a disclosure, closed unless one in it is
// set or on (decision 80).
var GROUPS = [
	{ members: [ 'square_production', 'clover_north_america' ] },
	{ title: _('More Clover regions'), members: [ 'clover_europe', 'clover_latin_america' ] },
	{ title: _('Sandbox (for developers)'), members: [ 'square_sandbox', 'clover_sandbox' ] }
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

// A secret's line: its name, whether it is set, and (unless it is made on
// install) its Set, Replace and Clear buttons.
function secretRow(secret, set) {
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
}

// Whether the running Bell says the integration is on, from its status:
// true, false, or null when it isn't running.
function served(st, integration) {
	var p = (st.running && st.status && st.status.points_of_sale) || null;
	if (!p)
		return null;
	return ((p[integration.wire[0]] || {})[integration.wire[1]]) === 'ready';
}

// An integration's card: whether it is on, its application, its API
// address and its secret. Its section is made with Add when the config
// doesn't have it yet, and Delete turns the integration off.
function integrationCard(m, st, sid) {
	var integration = INTEGRATIONS[sid], pos = integration.pos;
	var title = pos.title + ' ' + integration.name;
	var secret = { name: integration.secret, label: title + ' ' + pos.secret, help: pos.secretHelp };
	var s = m.section(form.NamedSection, sid, integration.wire[0], title);
	s.addremove = true;

	var o = s.option(form.DummyValue, '_state', _('State'));
	o.cfgvalue = function() {
		var on = served(st, integration);
		if (on == null)
			return _('The Bell isn’t running');
		return on ? _('On: the Bell serves it') : _('Off: set its application ID and secret to turn it on');
	};

	o = s.option(form.DummyValue, '_webhook', _('Webhook URL'),
		_('Where this environment’s webhooks go: enter it in the point of sale’s webhook settings for this application, exactly.'));
	o.cfgvalue = function() {
		var base = (uci.get(NAME, 'main', 'public_uri') || 'https://bell.example.org').replace(/\/+$/, '');
		return base + '/v1/' + integration.wire[0] + '/' + integration.wire[1] + '/webhook';
	};

	o = s.option(form.Value, 'application_id', pos.id, pos.idHelp);
	o.placeholder = integration.placeholder;

	o = s.option(form.Value, 'api_url', _('API address'),
		_('Where its API is, instead of the environment’s own: for a stand-in while developing. https, or http on this router. Leave empty.'));
	o.placeholder = integration.api;

	o = s.option(form.DummyValue, '_secret', _('Secret'));
	o.rawhtml = true;
	o.cfgvalue = function() {
		return secretRow(secret, st.secrets || {});
	};

	// Marked, so its card can be moved into its group once rendered.
	var render = s.render;
	s.render = function() {
		return Promise.resolve(render.apply(this, arguments)).then(function(node) {
			node.setAttribute('data-integration', sid);
			return node;
		});
	};
	return s;
}

// The rendered map with each group's cards together, the ones after the
// first inside a <details> that starts open when one of its integrations
// has an application ID or is on, so nothing set is hidden.
function grouped(st, node) {
	GROUPS.forEach(function(group) {
		var cards = group.members.map(function(sid) {
			return node.querySelector('[data-integration="' + sid + '"]');
		}).filter(function(card) { return card != null; });
		if (!group.title || !cards.length)
			return;
		var open = group.members.some(function(sid) {
			return !!uci.get(NAME, sid, 'application_id') || served(st, INTEGRATIONS[sid]) === true;
		});
		var details = E('details', open ? { 'open': '' } : {}, [
			E('summary', { 'style': 'cursor:pointer' }, [ E('h3', { 'style': 'display:inline' }, [ group.title ]) ])
		]);
		cards[0].parentNode.insertBefore(details, cards[0]);
		cards.forEach(function(card) { details.appendChild(card); });
	});
	return node;
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
		}

		o = s.option(form.DummyValue, '_secrets', _('Secrets'),
			_('Kept in <code>/etc/monokulo-bell/secrets/</code>, root’s alone, one file per secret. This page never shows them. Each integration’s is on its card below.'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return secretRow(VAPID, st.secrets || {});
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
			_('The https address the points of sale and the tills reach this Bell at, such as https://bell.example.org. Each integration’s webhooks go to it followed by /v1/<point of sale>/<environment>/webhook, as its card says; Square’s subscription must name that URL exactly. Required.'));
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

		// The integrations, a card each, in their groups' order: the Bell
		// serves every one whose application ID and secret are set, at once.
		GROUPS.forEach(function(group) {
			group.members.forEach(function(sid) { integrationCard(m, st, sid); });
		});

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

		// Grouped each time the map is drawn: at first, and again when a
		// Save redraws it.
		if (typeof m.renderContents === 'function') {
			var renderContents = m.renderContents;
			m.renderContents = function() {
				return Promise.resolve(renderContents.apply(this, arguments)).then(function(node) {
					return grouped(st, node);
				});
			};
			return m.render();
		}
		return m.render().then(function(node) { return grouped(st, node); });
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
