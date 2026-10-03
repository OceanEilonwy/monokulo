'use strict';
'require view';
'require form';
'require rpc';
'require uci';

// Services > Monokulo: whether monokulo (with the engine inside it) is
// running, and the settings it starts with. Everything else (Monero nodes, stores, abuse
// protection...) is on monokulo's own admin page.

var callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: [ 'name' ],
	expect: { '': {} }
});

function instances() {
	return L.resolveDefault(callServiceList('monokulo'), {}).then(function(res) {
		return (res && res.monokulo && res.monokulo.instances) || {};
	});
}

function state(instances, name) {
	var i = instances[name];
	return i && i.running ? _('Running') : _('Not running');
}

return view.extend({
	load: function() {
		return Promise.all([ uci.load('monokulo'), instances() ]);
	},

	render: function(data) {
		var running = data[1];
		var m, s, o;

		m = new form.Map('monokulo', _('Monokulo'),
			_('A self-hosted Monero payment gateway. Monokulo watches the chain with each store’s view key, never a spend key, and tells the store when an order is paid. Save & Apply restarts it with the new settings.'));

		s = m.section(form.NamedSection, 'main', 'monokulo', _('Status'));
		s.addremove = false;

		o = s.option(form.DummyValue, '_status', _('Monokulo'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			var port = parseInt(uci.get('monokulo', 'main', 'port'), 10) || 8081;
			var listen = uci.get('monokulo', 'main', 'listen') || 'lan';
			var text = '<strong>' + state(running, 'monokulo') + '</strong> &middot; ' + _('the engine runs inside it');
			if (running.monokulo && running.monokulo.running && listen !== 'loopback') {
				var url = 'http://' + window.location.hostname + ':' + port + '/';
				text += ' &middot; <a href="' + url + '" target="_blank" rel="noreferrer">' + _('Open monokulo') + '</a>';
			}
			return text;
		};

		o = s.option(form.DummyValue, '_secrets', _('Secrets'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return _('The encryption key for monokulo’s data is in <code>/etc/monokulo/secrets</code>, made on first start. <strong>Back it up</strong> (for example <code>scp root@router:/etc/monokulo/secrets .</code>): without it, monokulo’s data can’t be read.');
		};

		s = m.section(form.NamedSection, 'main', 'monokulo', _('Settings'));
		s.addremove = false;

		o = s.option(form.Flag, 'enabled', _('Enabled'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Value, 'port', _('Port'),
			_('TCP port monokulo listens on. To let customers outside your network reach the checkout, allow this port with a rule under Network › Firewall › Traffic Rules (not Port Forwards).'));
		o.datatype = 'port';
		o.placeholder = '8081';
		o.rmempty = false;

		o = s.option(form.ListValue, 'listen', _('Listen on'));
		o.value('lan', _('LAN only'));
		o.value('all', _('All interfaces'));
		o.value('loopback', _('This router only (127.0.0.1), e.g. behind tor'));
		o.default = 'lan';

		o = s.option(form.Value, 'data_dir', _('Data folder'),
			_('Databases, logs and the options file monokulo’s admin page saves. Must be on storage, not /tmp. Kept across firmware upgrades if left at the default.'));
		o.placeholder = '/srv/monokulo';
		o.validate = function(section_id, value) {
			if (!value)
				return true;
			if (!/^\/[^\s]*$/.test(value))
				return _('Use an absolute path such as /srv/monokulo.');
			if (/^\/(tmp|var)(\/|$)/.test(value))
				return _('/tmp and /var are in memory: everything there is lost on reboot.');
			return true;
		};

		o = s.option(form.Value, 'engine_cpus', _('Engine CPUs'),
			_('Which CPUs the engine’s threads inside monokulo may use, e.g. <code>2,3</code>. It runs one scan per CPU it is given, so leaving some out keeps them free for routing while it catches up with the chain. Empty: all of them.'));
		o.placeholder = '2,3';
		o.validate = function(section_id, value) {
			if (!value || /^[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*$/.test(value))
				return true;
			return _('A list such as 2,3 or 1-3.');
		};

		o = s.option(form.Value, 'engine_nice', _('Engine priority (nice)'),
			_('0 is normal, 19 the lowest. Higher gives way to everything else the router does, monokulo’s own web pages included.'));
		o.datatype = 'range(0,19)';
		o.placeholder = '10';

		return m.render();
	}
});
