#!/bin/sh
# Boot OpenWrt (the official x86_64 rootfs image, its own procd, ubus, rpcd
# and netifd) in Docker and test the packages scripts/build-openwrt.sh
# built for x86_64, as a router runs them: installed by name from the
# signed repository, their settings in UCI written into each options file,
# the services started, --check refusing a bad value, the LuCI pages'
# backend answering over ubus (through LuCI's own JSON-RPC, so the pages'
# ACLs are what is tested), an upgrade keeping settings and secrets, the
# relay refusing to run beside monokulo's own, and removal stopping each.
#
# Usage: scripts/test-openwrt.sh [dist]
#   dist: what scripts/build-openwrt.sh made for x86_64 (default dist/):
#   the repository in dist/repo/, and dist/throwaway.pem when a throwaway
#   key signed it (otherwise openwrt/keys/monokulo.pem must have).
#
# Needs: docker (the container runs --privileged: procd as PID 1 needs it).
# openwrt.yml's test job runs it after the x86_64 build.
set -eu
cd "$(dirname "$0")/.."

DIST=$(cd "${1:-dist}" && pwd)
IMAGE="${ROOTFS_IMAGE:-openwrt/rootfs:x86-64-25.12.5}"
C=monokulo-openwrt-test-$$
KEY=$DIST/throwaway.pem
[ -f "$KEY" ] || KEY=$PWD/openwrt/keys/monokulo.pem
for f in "$DIST/repo/packages.adb" "$KEY"; do
	[ -f "$f" ] || {
		echo "error: no $f: run scripts/build-openwrt.sh for x86_64 first" >&2
		exit 1
	}
done
# An APNs key (a P-256 key in PKCS#8, as Apple's AuthKey_<id>.p8 is) for
# the relay's app: made here, as no real one is needed to start it.
APNS_KEY=$(openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 2>/dev/null)

passed=0
cleanup() {
	docker rm -f "$C" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# x <script>: runs a shell script in the router.
x() {
	docker exec -i "$C" sh -c "$1"
}

fail() {
	echo "not ok - $1" >&2
	echo "--- the router's log:" >&2
	x 'logread | tail -n 60' >&2 || true
	exit 1
}

# check <what> <script>: the script, run in the router, must succeed.
check() {
	x "$2" >/dev/null 2>&1 || {
		echo "--- $2" >&2
		x "$2" >&2 || true
		fail "$1"
	}
	passed=$((passed + 1))
	echo "ok - $1"
}

# eventually <what> <script>: the script must succeed within 30 seconds.
eventually() {
	i=0
	until x "$2" >/dev/null 2>&1; do
		i=$((i + 1))
		[ $i -lt 60 ] || {
			echo "--- $2" >&2
			x "$2" >&2 || true
			fail "$1"
		}
		sleep 0.5
	done
	passed=$((passed + 1))
	echo "ok - $1"
}

docker run -d --privileged --name "$C" -v "$DIST:/dist:ro" "$IMAGE" /sbin/init >/dev/null
eventually "OpenWrt boots: procd, ubus, rpcd and LuCI's ubus objects" 'ubus call system board && ubus -S list luci'
eventually "the LAN has its address" '. /lib/functions/network.sh && network_get_ipaddr a lan && [ -n "$a" ]'

# Shell helpers in the router: whether a service runs, what the rpcd
# plugin and LuCI's JSON-RPC say, and a session for the latter.
x 'cat >/tmp/t.sh' <<'EOF'
running() {
	ubus call service list "{\"name\":\"$1\"}" | jsonfilter -q -e "@['$1'].instances.*.running" | grep -q true
}
pid() {
	ubus call service list "{\"name\":\"$1\"}" | jsonfilter -q -e "@['$1'].instances.*.pid"
}
# rpc <object> <method> [args]: through uhttpd's /ubus, as LuCI calls it,
# in a session of a login granted the object's LuCI page's ACL alone
# (luci-app-<object>), so the call is held to what the page may do.
rpc() {
	args=${3:-'{}'}
	sid=$(ubus call session login "{\"username\":\"$1\",\"password\":\"\"}" | jsonfilter -e '@.ubus_rpc_session')
	uclient-fetch -q -O - --post-data="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"call\",\"params\":[\"$sid\",\"$1\",\"$2\",$args]}" \
		http://127.0.0.1/ubus | jsonfilter -e '@.result[1]'
}
# denied <object> <method>: the page's ACL doesn't let it call that.
denied() {
	sid=$(ubus call session login "{\"username\":\"$1\",\"password\":\"\"}" | jsonfilter -e '@.ubus_rpc_session')
	[ "$(uclient-fetch -q -O - --post-data="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"call\",\"params\":[\"$sid\",\"$2\",\"$3\",{}]}" \
		http://127.0.0.1/ubus | jsonfilter -e '@.error.code')" = -32002 ]
}
EOF

# The repository, as the router's install page says: its key trusted and
# the repository added, nothing else (the container has no internet: the
# packages' dependencies are in the image already).
docker cp "$KEY" "$C:/etc/apk/keys/monokulo.pem"
x ': >/etc/apk/repositories.d/distfeeds.list; echo /dist/repo/packages.adb >/etc/apk/repositories.d/customfeeds.list'
check "the repository's index is signed by the key the router trusts" 'apk update'
check "the repository holds the three packages" 'apk search monokulo | grep -q "^monokulo-[0-9]" && apk search monokulo | grep -q "^monokulo-push-relay-" && apk search monokulo | grep -q "^monokulo-bell-"'

# luci_login <package>: a LuCI login granted the package's page's ACL
# alone, named as the package (root's password, none here), which rpc and
# denied call as; that its status answers, and that the ACL grants nothing
# beyond the page.
luci_login() {
	pkg=$1
	x "uci -q batch >/dev/null; /etc/init.d/rpcd reload" <<-EOF
		add rpcd login
		set rpcd.@login[-1].username='$pkg'
		set rpcd.@login[-1].password='\$p\$root'
		add_list rpcd.@login[-1].read='luci-app-$pkg'
		add_list rpcd.@login[-1].write='luci-app-$pkg'
		commit rpcd
	EOF
	eventually "LuCI's JSON-RPC answers $pkg's status under its page's ACL" ". /tmp/t.sh; rpc $pkg status | jsonfilter -e '@.secrets' >/dev/null"
	check "$pkg's page's ACL grants nothing beyond it (not procd's service list, which holds the secrets, nor LuCI's setInitAction, which takes any service)" \
		". /tmp/t.sh; denied $pkg service list && denied $pkg file read && denied $pkg luci setInitAction"
}

# --- monokulo, as before: install, start, upgrade, removal --------------------

check "monokulo installs by name from the repository" 'apk add monokulo'
eventually "monokulo runs" '. /tmp/t.sh; running monokulo'
check "rpcd serves monokulo's ubus object" 'ubus -S list monokulo'
luci_login monokulo
check "LuCI's status of monokulo has it running and its build, and no secret" \
	'. /tmp/t.sh; s=$(rpc monokulo status); [ "$(echo "$s" | jsonfilter -e @.running)" = true ] && echo "$s" | jsonfilter -e @.running_build.version | grep -q -- -r && ! echo "$s" | grep -q "$(sed -n "s/^MONOKULO_ENCRYPTION_KEY=//p" /etc/monokulo/secrets)" && ! rpc monokulo check | grep -q ok'
check "LuCI restarts monokulo through its backend" \
	'. /tmp/t.sh; pid monokulo >/tmp/monokulo.pid; [ "$(rpc monokulo restart | jsonfilter -e @.result)" = true ]'
eventually "monokulo runs again after LuCI's restart, a new process" \
	'. /tmp/t.sh; running monokulo && [ "$(pid monokulo)" != "$(cat /tmp/monokulo.pid)" ]'
check "monokulo's encryption key was made, root's alone" '[ "$(ls -ln /etc/monokulo/secrets | cut -c1-10)" = "-rw-------" ] && grep -q "^MONOKULO_ENCRYPTION_KEY=[0-9a-f]\{64\}$" /etc/monokulo/secrets'
x 'uci set monokulo.main.engine_nice=12; uci commit monokulo; sha256sum /etc/monokulo/secrets >/tmp/monokulo.sum'
check "monokulo's upgrade keeps its settings and its key" 'apk fix --reinstall monokulo && [ "$(uci get monokulo.main.engine_nice)" = 12 ] && sha256sum -c /tmp/monokulo.sum'
eventually "monokulo runs after its upgrade" '. /tmp/t.sh; running monokulo'

# --- the relay and the Bell -----------------------------------------------------

# service_basics <package> <generated secret> <port>
service_basics() {
	pkg=$1 generated=$2 port=$3
	check "$pkg installs by name from the repository" "apk add $pkg"
	check "$pkg's user exists" "id -u $pkg"
	check "$pkg's $generated was made on install, 64 hex characters, root's alone" \
		"[ \"\$(ls -ln /etc/$pkg/secrets/$generated | awk '{ print \$1, \$3 }')\" = '-rw------- 0' ] && grep -qx '[0-9a-f]\{64\}' /etc/$pkg/secrets/$generated && [ \"\$(ls -ldn /etc/$pkg/secrets | cut -c1-10)\" = drwx------ ]"
	check "$pkg's LuCI page is installed: its view, menu and ACL" \
		"[ -s /www/luci-static/resources/view/$pkg.js ] && jsonfilter -i /usr/share/luci/menu.d/luci-app-$pkg.json -e '@[\"admin/services/$pkg\"].action.path' | grep -qx $pkg && jsonfilter -i /usr/share/rpcd/acl.d/luci-app-$pkg.json -e '@[\"luci-app-$pkg\"].read.ubus[\"$pkg\"]' | grep -q status"
	check "rpcd serves $pkg's ubus object" "ubus -S list $pkg"
	luci_login "$pkg"
	check "$pkg's firewall rule is made, off" \
		"[ \"\$(uci get firewall.$(echo "$pkg" | tr - _).enabled)\" = 0 ] && [ \"\$(uci get firewall.$(echo "$pkg" | tr - _).dest_port)\" = $port ]"
	check "$pkg isn't running before it has a public address, and says why" \
		". /tmp/t.sh; ! running $pkg && grep -q public_uri /tmp/$pkg.check && rpc $pkg status | jsonfilter -e '@.check_error' | grep -q public_uri"
	check "the sysupgrade backup keeps $pkg's settings, secrets and data" \
		"sysupgrade -l | grep -qx /etc/config/$pkg && sysupgrade -l | grep -qx /etc/$pkg/secrets/$generated"
}

# service_runs <package> <options file> <status port> <listen>
service_runs() {
	pkg=$1 options=$2 status=$3 listen=$4
	check "LuCI's check of $pkg passes" ". /tmp/t.sh; [ \"\$(rpc $pkg check | jsonfilter -e '@.ok')\" = true ]"
	check "LuCI starts $pkg through its backend" ". /tmp/t.sh; [ \"\$(rpc $pkg restart | jsonfilter -e '@.result')\" = true ]"
	eventually "$pkg runs, as its own user" \
		". /tmp/t.sh; running $pkg && [ \"\$(awk '/^Uid:/ { print \$2 }' /proc/\$(pid $pkg)/status)\" = \"\$(id -u $pkg)\" ]"
	check "$pkg's options file holds the UCI settings" "grep -qx 'status.listen = \"127.0.0.1:$status\"' $options"
	eventually "$pkg's status listener answers on this router" \
		"uclient-fetch -q -O - http://127.0.0.1:$status/status | jsonfilter -e '@.service' | grep -qx $pkg"
	check "$pkg listens where UCI says" \
		"uclient-fetch -q -O - http://127.0.0.1:$status/status | jsonfilter -e '@.listen' | grep -qx '$listen'"
	check "LuCI's status of $pkg has it running, its build and its own status" \
		". /tmp/t.sh; s=\$(rpc $pkg status); [ \"\$(echo \"\$s\" | jsonfilter -e '@.running')\" = true ] && echo \"\$s\" | jsonfilter -e '@.status.service' | grep -qx $pkg && echo \"\$s\" | jsonfilter -e '@.running_build.version' | grep -q -- -r && [ -z \"\$(echo \"\$s\" | jsonfilter -q -e '@.check_error')\" ]"
	check "LuCI's status of $pkg names no secret's value" \
		". /tmp/t.sh; s=\$(rpc $pkg status); for f in /etc/$pkg/secrets/*; do v=\$(head -c 24 \$f); case \"\$s\" in *\"\$v\"*) exit 1 ;; esac; done"
}

# refuses_bad_value <package> <option> <bad value> <what the binary says>
refuses_bad_value() {
	pkg=$1 option=$2 bad=$3 says=$4
	x ". /tmp/t.sh; pid $pkg >/tmp/$pkg.pid; uci -q get $pkg.main.$option >/tmp/$pkg.good; uci set $pkg.main.$option='$bad'; uci commit $pkg"
	check "LuCI's check of $pkg refuses $option=$bad, in $pkg's words" \
		". /tmp/t.sh; r=\$(rpc $pkg check); [ \"\$(echo \"\$r\" | jsonfilter -e '@.ok')\" = false ] && echo \"\$r\" | jsonfilter -e '@.output' | grep -q '$says'"
	check "a reload with $option=$bad leaves $pkg running on its earlier settings" \
		". /tmp/t.sh; /etc/init.d/$pkg reload; sleep 2; running $pkg && [ \"\$(pid $pkg)\" = \"\$(cat /tmp/$pkg.pid)\" ] && grep -q '$says' /tmp/$pkg.check"
	check "a restart with $option=$bad doesn't start $pkg, and LuCI says why, without the file the init script wrote" \
		". /tmp/t.sh; /etc/init.d/$pkg restart; sleep 1; ! running $pkg && e=\$(rpc $pkg status | jsonfilter -e '@.check_error') && echo \"\$e\" | grep -q '$says' && ! echo \"\$e\" | grep -q '\\.toml\\|/tmp/\\|line [0-9]'"
	x "if [ -s /tmp/$pkg.good ]; then uci set $pkg.main.$option=\"\$(cat /tmp/$pkg.good)\"; else uci delete $pkg.main.$option; fi; uci commit $pkg; /etc/init.d/$pkg restart"
	eventually "$pkg runs again with $option restored, and the refusal is gone" \
		". /tmp/t.sh; running $pkg && [ ! -e /tmp/$pkg.check ]"
}

# refuses_data_dir <package>: a data folder in memory, or one holding
# another's files, is refused with its reason for LuCI: a reload keeps
# the service on its earlier settings, a restart doesn't start it.
refuses_data_dir() {
	pkg=$1
	x ". /tmp/t.sh; pid $pkg >/tmp/$pkg.pid; uci set $pkg.main.data_dir=/tmp/$pkg-data; uci commit $pkg"
	check "a reload with its data folder in memory leaves $pkg running on its earlier settings, and LuCI says why" \
		". /tmp/t.sh; /etc/init.d/$pkg reload; sleep 2; running $pkg && [ \"\$(pid $pkg)\" = \"\$(cat /tmp/$pkg.pid)\" ] && rpc $pkg status | jsonfilter -e '@.check_error' | grep -q 'in memory'"
	check "a restart with its data folder in memory doesn't start $pkg, and LuCI says why" \
		". /tmp/t.sh; /etc/init.d/$pkg restart; sleep 1; ! running $pkg && rpc $pkg status | jsonfilter -e '@.check_error' | grep -q 'in memory'"
	x "mkdir -p /root/$pkg-other && touch /root/$pkg-other/theirs && uci set $pkg.main.data_dir=/root/$pkg-other && uci commit $pkg"
	check "a restart with a data folder holding another's files doesn't start $pkg nor take the folder, and LuCI says why" \
		". /tmp/t.sh; /etc/init.d/$pkg restart; sleep 1; ! running $pkg && [ \"\$(ls -ldn /root/$pkg-other | awk '{ print \$3 }')\" = 0 ] && rpc $pkg status | jsonfilter -e '@.check_error' | grep -q 'holds other files'"
	x "rm -r /root/$pkg-other; uci set $pkg.main.data_dir=/srv/$pkg; uci commit $pkg; /etc/init.d/$pkg restart"
	eventually "$pkg runs again in its own data folder, and the refusal is gone" \
		". /tmp/t.sh; running $pkg && [ ! -e /tmp/$pkg.check ]"
}

# upgrade_keeps <package> <option>
upgrade_keeps() {
	pkg=$1 option=$2
	x "sha256sum /etc/$pkg/secrets/* >/tmp/$pkg.sums; uci get $pkg.main.$option >/tmp/$pkg.value"
	check "$pkg's upgrade keeps its settings and secrets" \
		"apk fix --reinstall $pkg && [ \"\$(uci get $pkg.main.$option)\" = \"\$(cat /tmp/$pkg.value)\" ] && sha256sum -c /tmp/$pkg.sums"
	eventually "$pkg runs after its upgrade" ". /tmp/t.sh; running $pkg"
}

# removal_stops <package>
# removal_stops <package>: apk del stops the service and takes what the
# package installed. The prerm's stop returns while procd is still ending
# the process, so the stop is waited for, not read straight after.
removal_stops() {
	pkg=$1
	check "$pkg is removed" "apk del $pkg"
	eventually "$pkg's removal stops it" ". /tmp/t.sh; ! running $pkg"
	check "$pkg's removal takes its program, LuCI page and firewall rule" \
		"[ ! -e /usr/bin/$pkg ] && [ ! -e /www/luci-static/resources/view/$pkg.js ] && ! uci -q get firewall.$(echo "$pkg" | tr - _) && ! ubus -S list $pkg"
	check "$pkg's removal keeps its data and secrets, the operator's" "[ -d /srv/$pkg ] && ls /etc/$pkg/secrets | grep -q ."
}

# The relay: its one app on APNs, the key uploaded as LuCI does.
service_basics monokulo-push-relay MONOKULO_RELAY_HANDLE_KEY 8444
x "uci -q batch >/dev/null" <<'EOF'
set monokulo-push-relay.main.public_uri='https://relay.example.org'
set monokulo-push-relay.main.listen='all'
add_list monokulo-push-relay.main.trusted_proxy='127.0.0.1'
add_list monokulo-push-relay.main.trusted_proxy='10.0.0.0/8'
set monokulo-push-relay.main.apns_team_id='ABCDE12345'
set monokulo-push-relay.main.apns_key_id='FGHIJ67890'
add monokulo-push-relay app
set monokulo-push-relay.@app[-1].name='monokular'
set monokulo-push-relay.@app[-1].apns_topic='com.example.monokular'
commit monokulo-push-relay
EOF
key_json=$(printf '%s' "$APNS_KEY" | awk 'BEGIN { ORS = "" } { if (NR > 1) print "\\n"; print }')
check "LuCI uploads the APNs key, and its status says it is set" \
	". /tmp/t.sh; [ \"\$(rpc monokulo-push-relay set_secret '{\"name\":\"MONOKULO_RELAY_APNS_KEY\",\"value\":\"$key_json\"}' | jsonfilter -e '@.ok')\" = true ] && [ \"\$(rpc monokulo-push-relay status | jsonfilter -e '@.secrets.MONOKULO_RELAY_APNS_KEY')\" = true ] && [ \"\$(ls -ln /etc/monokulo-push-relay/secrets/MONOKULO_RELAY_APNS_KEY | cut -c1-10)\" = -rw------- ]"
check "LuCI can't replace the handle key" \
	". /tmp/t.sh; [ \"\$(rpc monokulo-push-relay set_secret '{\"name\":\"MONOKULO_RELAY_HANDLE_KEY\",\"value\":\"x\"}' | jsonfilter -e '@.ok')\" = false ]"
check "LuCI can't write a file that isn't a secret" \
	". /tmp/t.sh; [ \"\$(rpc monokulo-push-relay set_secret '{\"name\":\"../../config/x\",\"value\":\"x\"}' | jsonfilter -e '@.ok')\" = false ] && [ ! -e /etc/config/x ]"
service_runs monokulo-push-relay /var/etc/monokulo-push-relay/relay.toml 8446 0.0.0.0:8444
check "the relay's options file holds every UCI setting, the list and the app" \
	"f=/var/etc/monokulo-push-relay/relay.toml; grep -qx 'server.public_uri = \"https://relay.example.org\"' \$f && grep -qx 'server.trusted_proxies = \"127.0.0.1, 10.0.0.0/8\"' \$f && grep -qx 'handles.apps.\"monokular\".apns_topic = \"com.example.monokular\"' \$f && grep -qx 'store.path = \"/srv/monokulo-push-relay/push-relay.db\"' \$f"
check "the relay's status says APNs is ready, for its app" \
	"s=\$(uclient-fetch -q -O - http://127.0.0.1:8446/status); [ \"\$(echo \"\$s\" | jsonfilter -e '@.platforms.apns')\" = ready ] && [ \"\$(echo \"\$s\" | jsonfilter -e '@.apps[0]')\" = monokular ]"
refuses_bad_value monokulo-push-relay public_uri 'http://relay.example.org' https
# A value TOML would choke on reaches the relay as written: its own error.
refuses_bad_value monokulo-push-relay log_level 'info,"x\y' logging.level
refuses_data_dir monokulo-push-relay
upgrade_keeps monokulo-push-relay public_uri

# The relay refuses to run beside monokulo's own (relay.enabled).
x 'cp -p /srv/monokulo/monokulo.toml /tmp/monokulo.toml 2>/dev/null; printf "\n[relay]\nenabled = true\n" >>/srv/monokulo/monokulo.toml'
check "the relay doesn't start while monokulo's own relay is on, and LuCI says why" \
	". /tmp/t.sh; /etc/init.d/monokulo-push-relay restart; sleep 1; ! running monokulo-push-relay && rpc monokulo-push-relay status | jsonfilter -e '@.blocked_by' | grep -q \"monokulo's own push relay is on\" && /etc/init.d/monokulo-push-relay check | grep -q 8444"
x 'if [ -e /tmp/monokulo.toml ]; then cp -p /tmp/monokulo.toml /srv/monokulo/monokulo.toml; else rm /srv/monokulo/monokulo.toml; fi; /etc/init.d/monokulo-push-relay restart'
eventually "the relay starts again once monokulo's relay is off" ". /tmp/t.sh; running monokulo-push-relay && [ -z \"\$(rpc monokulo-push-relay status | jsonfilter -q -e '@.blocked_by')\" ]"
check "monokulo's relay.enabled is read from its own table only" \
	". /etc/init.d/monokulo-push-relay; printf '[relay.server]\nenabled = true\n[other]\nenabled = true\n# [relay]\n' >/tmp/r.toml; ! monokulo_relay_on /tmp/r.toml && printf '[ relay ]\n  enabled = true # on\n' >/tmp/r.toml && monokulo_relay_on /tmp/r.toml && printf '[relay]\nenabled = false\n' >/tmp/r.toml && ! monokulo_relay_on /tmp/r.toml"

# The Bell: Square's sandbox on, its webhook key set as LuCI does.
service_basics monokulo-bell MONOKULO_BELL_VAPID_KEY 8445
x "uci -q batch >/dev/null" <<'EOF'
set monokulo-bell.main.public_uri='https://bell.example.org'
set monokulo-bell.main.allow_private_endpoints='0'
set monokulo-bell.square_sandbox.application_id='sandbox-sq0idb-abcdefghijklmnopqrstuv'
commit monokulo-bell
EOF
check "LuCI's check of the Bell says Square's sandbox needs its webhook key" \
	". /tmp/t.sh; rpc monokulo-bell check | jsonfilter -e '@.output' | grep -q MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY"
check "LuCI sets Square sandbox's webhook key, and its status says it is set" \
	". /tmp/t.sh; [ \"\$(rpc monokulo-bell set_secret '{\"name\":\"MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY\",\"value\":\"a-signature-key\"}' | jsonfilter -e '@.ok')\" = true ] && [ \"\$(rpc monokulo-bell status | jsonfilter -e '@.secrets.MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY')\" = true ]"
service_runs monokulo-bell /var/etc/monokulo-bell/bell.toml 8447 127.0.0.1:8445
check "the Bell's options file holds the UCI settings, Square sandbox's in its table" \
	"f=/var/etc/monokulo-bell/bell.toml; grep -qx 'bell.square.sandbox.application_id = \"sandbox-sq0idb-abcdefghijklmnopqrstuv\"' \$f && grep -qx 'bell.allow_private_endpoints = \"false\"' \$f && grep -qx 'bell.store_path = \"/srv/monokulo-bell/bell.db\"' \$f && ! grep -q clover \$f"
check "the Bell's status says Square's sandbox is on and the others off" \
	"s=\$(uclient-fetch -q -O - http://127.0.0.1:8447/status); [ \"\$(echo \"\$s\" | jsonfilter -e '@.points_of_sale.square.sandbox')\" = ready ] && [ \"\$(echo \"\$s\" | jsonfilter -e '@.points_of_sale.square.production')\" = off ] && [ \"\$(echo \"\$s\" | jsonfilter -e '@.points_of_sale.clover.north_america')\" = off ]"
refuses_bad_value monokulo-bell timeout_ms 50 bell.timeout_ms
refuses_data_dir monokulo-bell
check "LuCI clears a secret" \
	". /tmp/t.sh; [ \"\$(rpc monokulo-bell clear_secret '{\"name\":\"MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY\"}' | jsonfilter -e '@.ok')\" = true ] && [ ! -e /etc/monokulo-bell/secrets/MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY ] && [ \"\$(rpc monokulo-bell check | jsonfilter -e '@.ok')\" = false ]"
x '. /tmp/t.sh; rpc monokulo-bell set_secret "{\"name\":\"MONOKULO_BELL_SQUARE_SANDBOX_WEBHOOK_KEY\",\"value\":\"a-signature-key\"}" >/dev/null'
upgrade_keeps monokulo-bell public_uri
# An install from before the integrations: one section per point of sale,
# with its environment, and its secret under the old name.
x "uci -q batch >/dev/null" <<'EOF'
set monokulo-bell.clover=clover
set monokulo-bell.clover.application_id='MONOKULARBELL'
set monokulo-bell.clover.environment='europe'
commit monokulo-bell
EOF
x 'printf %s 9f2d4a8c-6b1e-4f3a-9c7d-2e5b8a1f4c6d >/etc/monokulo-bell/secrets/MONOKULO_BELL_CLOVER_AUTH_CODE'
check "an upgrade renames the old Clover section and its secret for its environment" \
	"apk fix --reinstall monokulo-bell && [ \"\$(uci get monokulo-bell.clover_europe)\" = clover ] && [ \"\$(uci get monokulo-bell.clover_europe.application_id)\" = MONOKULARBELL ] && ! uci -q get monokulo-bell.clover && ! uci -q get monokulo-bell.clover_europe.environment && [ -s /etc/monokulo-bell/secrets/MONOKULO_BELL_CLOVER_EUROPE_AUTH_CODE ] && [ ! -e /etc/monokulo-bell/secrets/MONOKULO_BELL_CLOVER_AUTH_CODE ]"
eventually "the Bell runs on the renamed section, Clover europe on" \
	". /tmp/t.sh; running monokulo-bell && [ \"\$(uclient-fetch -q -O - http://127.0.0.1:8447/status | jsonfilter -e '@.points_of_sale.clover.europe')\" = ready ]"

# --- removal -----------------------------------------------------------------

removal_stops monokulo-push-relay
removal_stops monokulo-bell
check "monokulo is removed" 'apk del monokulo'
eventually "monokulo's removal stops it" '. /tmp/t.sh; ! running monokulo'
check "monokulo's removal takes its program and keeps its key" '[ ! -e /usr/bin/monokulo ] && [ -s /etc/monokulo/secrets ]'

echo "all $passed checks passed"
