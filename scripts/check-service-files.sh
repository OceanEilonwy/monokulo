#!/usr/bin/env bash
# Installs a release zip's service file the way its header says, on the
# runner it was built for, starts the service, reads its status, then stops
# and removes it: so the systemd unit, the launchd plist and the Windows task
# script are known to start the binary they ship with. release.yml runs it
# after packaging, as root on Linux and macOS (sudo) and as an administrator
# on Windows.
#
# Usage: scripts/check-service-files.sh NAME DIR
#   NAME  monokulo-push-relay or monokulo-bell
#   DIR   the unzipped folder: the binary, its starter options file, service/
set -euo pipefail

name=$1
dir=$(cd "$2" && pwd)

# The least each needs to start on this machine: an http address is taken
# for loopback, and a sandbox Square application for the Bell.
case "$name" in
monokulo-push-relay)
	options=relay.toml
	status=8446
	settings='[server]
listen = "127.0.0.1:8444"
public_uri = "http://127.0.0.1:8444"'
	secrets=("MONOKULO_RELAY_HANDLE_KEY=$(openssl rand -hex 32)")
	unit=monokulo-push-relay
	plist=com.monokulo.push-relay
	;;
monokulo-bell)
	options=bell.toml
	status=8447
	settings='[bell]
public_uri = "http://127.0.0.1:8445"

[bell.square]
application_id = "sandbox-sq0idb-release-check"
environment = "sandbox"'
	secrets=("MONOKULO_BELL_VAPID_KEY=$(openssl rand -hex 32)" "MONOKULO_BELL_SQUARE_WEBHOOK_KEY=release-check")
	unit=monokulo-bell
	plist=com.monokulo.bell
	;;
*)
	echo "unknown service $name" >&2
	exit 2
	;;
esac

# The options file: the settings above alone (the zip's starter file has
# them commented out, and its tables already open).
starter() {
	printf '%s\n' "$settings"
}

# The status, once it answers (up to 60 s), naming the service.
wait_for_status() {
	for _ in $(seq 60); do
		if body=$(curl -fsS "http://127.0.0.1:$status/status" 2> /dev/null); then
			echo "$body"
			grep -q "\"service\":\"$name\"" <<< "$body"
			return
		fi
		sleep 1
	done
	echo "$name's status never answered on 127.0.0.1:$status" >&2
	return 1
}

# The status gone (up to 30 s), once the service is stopped.
wait_for_stop() {
	for _ in $(seq 30); do
		curl -fsS "http://127.0.0.1:$status/status" > /dev/null 2>&1 || return 0
		sleep 1
	done
	echo "$name still answers on 127.0.0.1:$status after it was stopped" >&2
	return 1
}

case "$(uname -s)" in
Linux)
	sudo install -m 755 "$dir/$name" /usr/local/bin/
	sudo install -d /etc/monokulo
	starter | sudo tee "/etc/monokulo/$options" > /dev/null
	sudo install -m 600 /dev/null "/etc/monokulo/$name.env"
	for secret in "${secrets[@]}"; do
		printf "%s='%s'\n" "${secret%%=*}" "${secret#*=}" | sudo tee -a "/etc/monokulo/$name.env" > /dev/null
	done
	sudo install -m 644 "$dir/service/$unit.service" /etc/systemd/system/
	sudo systemctl daemon-reload
	if ! sudo systemctl start "$unit" || ! wait_for_status; then
		sudo systemctl status "$unit" --no-pager || true
		sudo journalctl -u "$unit" --no-pager || true
		exit 1
	fi
	sudo systemctl stop "$unit"
	wait_for_stop
	sudo rm "/etc/systemd/system/$unit.service" "/etc/monokulo/$name.env" "/etc/monokulo/$options" "/usr/local/bin/$name"
	sudo systemctl daemon-reload
	;;
Darwin)
	data="/Library/Application Support/monokulo"
	sudo mkdir -p /usr/local/bin "$data/${name#monokulo-}"
	sudo install -m 755 "$dir/$name" /usr/local/bin/
	starter | sudo tee "$data/$options" > /dev/null
	sudo chown nobody "$data/${name#monokulo-}"
	sudo chmod 700 "$data/${name#monokulo-}"
	# The secrets in place of the REPLACE values; the ones not set here go.
	daemon="/Library/LaunchDaemons/$plist.plist"
	sudo install -m 600 -o root -g wheel "$dir/service/$plist.plist" "$daemon"
	sudo /usr/libexec/PlistBuddy -c 'Delete :EnvironmentVariables' -c 'Add :EnvironmentVariables dict' "$daemon"
	for secret in "${secrets[@]}"; do
		sudo /usr/libexec/PlistBuddy -c "Add :EnvironmentVariables:${secret%%=*} string ${secret#*=}" "$daemon"
	done
	sudo plutil -lint "$daemon"
	if ! sudo launchctl bootstrap system "$daemon" || ! wait_for_status; then
		sudo launchctl print "system/$plist" || true
		sudo cat "$data/${name#monokulo-}/${name#monokulo-}.log" || true
		exit 1
	fi
	sudo launchctl bootout "system/$plist"
	wait_for_stop
	sudo rm -rf "$daemon" "$data" "/usr/local/bin/$name"
	;;
MINGW* | MSYS*)
	# The task script copies the options file from the folder when there is
	# none; the secrets go in as files, all but the first (the one the
	# script makes on install).
	starter > "$dir/$options"
	data="$(cygpath -u "$PROGRAMDATA")/monokulo/$name"
	mkdir -p "$data/secrets"
	for secret in "${secrets[@]:1}"; do
		printf '%s' "${secret#*=}" > "$data/secrets/${secret%%=*}"
	done
	script=$(cygpath -w "$dir/service/install-windows-task.ps1")
	if ! powershell -NoProfile -ExecutionPolicy Bypass -File "$script" || ! wait_for_status; then
		powershell -NoProfile -Command "Get-ScheduledTask -TaskName '$name' | Get-ScheduledTaskInfo" || true
		cat "$data/logs/$name.log" || true
		exit 1
	fi
	test -s "$data/secrets/${secrets[0]%%=*}"
	powershell -NoProfile -ExecutionPolicy Bypass -File "$script" -Uninstall
	wait_for_stop
	if powershell -NoProfile -Command "Get-ScheduledTask -TaskName '$name' -ErrorAction Stop" > /dev/null 2>&1; then
		echo "the task $name is still registered after -Uninstall" >&2
		exit 1
	fi
	rm -rf "$data"
	;;
*)
	echo "no service file to check on $(uname -s)" >&2
	exit 2
	;;
esac
echo "$name started, answered its status and stopped as its service file runs it."
