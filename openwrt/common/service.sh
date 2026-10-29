# shellcheck shell=ash
# shellcheck disable=SC2034,SC3043
# What the init scripts of monokulo-push-relay and monokulo-bell share
# (each package installs its own copy as /usr/share/<package>/service.sh,
# which its init script sources): the secrets folder, the data folder, the
# options file written from UCI at each start and checked with the
# binary's own --check, and the procd instance.
#
# The init script sets, before sourcing it:
#   NAME            the package, its UCI config, its user and its binary
#   OPTIONS         the options file each start writes (in memory)
#   SECRETS         every secret's variable; GENERATED_SECRET, the one
#                   made on install
#   LISTEN_KEY      the setting the port and listen options make
#   STORE_KEY       the database file's setting; STORE_FILE its name in
#                   the data folder
#   STATUS_LISTEN   the loopback status listener LuCI reads
#   DEFAULT_PORT, DEFAULT_LISTEN
#   OPTIONS_MAP     the main section's settings (toml_options)
#   GENERATED_WHY   why the generated secret must be kept, for the log
#   PAGE            the LuCI page, for messages
# and defines render_more (more of the options file, from the loaded
# config) and blocked (prints why the service mustn't run here and
# succeeds, or fails).
#
# A bad setting never stops a running service: a reload (a save in LuCI,
# `uci commit`) that fails --check leaves it running on its last good
# options file, and the reason is in /tmp/<package>.check for LuCI to
# show. A start or restart that fails --check doesn't start it.

BIN=/usr/bin/$NAME
RUN_AS=$NAME
SECRETS_DIR=/etc/$NAME/secrets
DEFAULT_DATA_DIR=/srv/$NAME
# What the last start refused, for LuCI; and what blocks it.
CHECK_FILE=/tmp/$NAME.check
BLOCKED_FILE=/tmp/$NAME.blocked
# Which build the package installed (its Makefile writes it).
VERSION_FILE=/usr/share/$NAME/version

log() {
	logger -t "$NAME" "$*"
}

random_hex() {
	hexdump -vn32 -e '32/1 "%02x"' /dev/urandom
}

# toml_string <value>: the value as a TOML basic string. No setting takes
# a control character, so one is an error.
toml_string() {
	case "$1" in
	*[[:cntrl:]]*) return 1 ;;
	esac
	printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g')"
}

# toml_set <key> <value>: one setting, by its dotted key (a table each
# part), so the file needs no table headers and any order is valid. A
# value is always a TOML string: the binary parses it, and a wrong one is
# its error, as --check says it.
toml_set() {
	local value
	value=$(toml_string "$2") || {
		echo "$1 holds a control character: set it again without one" >&2
		return 1
	}
	printf '%s = %s\n' "$1" "$value"
}

# toml_options <section> <map>: for each line "<setting key> <UCI option>
# <kind>" of map whose option is set in section, the setting under its
# table. Only what is set is written, so the binary's own defaults rule
# the rest. Kinds: value (as it is), list (a UCI list, comma separated)
# and flag (UCI's 0/1 as false/true).
toml_options() {
	local section="$1" key option kind value
	while read -r key option kind; do
		[ -n "$key" ] || continue
		config_get value "$section" "$option"
		[ -n "$value" ] || continue
		case "$kind" in
		list) value=$(echo "$value" | sed 's/  */, /g') ;;
		flag)
			config_get_bool value "$section" "$option"
			if [ "$value" = 1 ]; then value=true; else value=false; fi
			;;
		esac
		toml_set "$key" "$value" || return 1
	done <<-EOF
		$2
	EOF
}

# The generated secret, made once and never replaced; the folder and every
# file in it root's alone.
secrets() {
	local file="$SECRETS_DIR/$GENERATED_SECRET"
	mkdir -p "$SECRETS_DIR" || return 1
	chown root:root "${SECRETS_DIR%/*}" "$SECRETS_DIR"
	chmod 700 "${SECRETS_DIR%/*}" "$SECRETS_DIR"
	if [ ! -s "$file" ]; then
		(
			umask 077
			random_hex >"$file.new"
		) && mv "$file.new" "$file" || return 1
		log "made $file. Back it up: $GENERATED_WHY"
	fi
	for file in "$SECRETS_DIR"/*; do
		[ -f "$file" ] || continue
		chown root:root "$file"
		chmod 600 "$file"
	done
}

# Runs a command with each secret that is set in its environment.
with_secrets() {
	(
		for secret in $SECRETS; do
			[ -s "$SECRETS_DIR/$secret" ] && export "$secret=$(cat "$SECRETS_DIR/$secret")"
		done
		exec "$@"
	)
}

# Use the data folder only if it is the service's own: new, empty, or
# already owned by its user. Never take over some other directory.
prepare_data_dir() {
	local dir="$1"
	case "$dir" in
	/tmp/* | /var/*) log "the data folder $dir is in memory and would be lost on reboot; choose one on storage"; return 1 ;;
	/*) ;;
	*) log "the data folder must be an absolute path, not '$dir'"; return 1 ;;
	esac
	[ -e "$dir" ] || mkdir -p "$dir" || return 1
	if [ ! -d "$dir" ]; then
		log "the data folder $dir is not a folder"
		return 1
	fi
	if [ "$(ls -ldn "$dir" | awk '{ print $3 }')" != "$(id -u "$RUN_AS")" ] && [ -n "$(ls -A "$dir")" ]; then
		log "not using $dir: it holds other files and isn't $NAME's own folder; choose a new or empty folder in $PAGE"
		return 1
	fi
	chown -R "$RUN_AS:$RUN_AS" "$dir"
	chmod 700 "$dir"
}

# The address to listen on: the LAN's, every interface's or this router's.
listen_addr() {
	local addr
	case "$1" in
	all) addr=0.0.0.0 ;;
	loopback) addr=127.0.0.1 ;;
	*)
		. /lib/functions/network.sh
		network_get_ipaddr addr lan || return 1
		;;
	esac
	echo "$addr"
}

# render_options <file>: the options file, from the loaded config.
render_options() {
	local port listen data_dir addr
	config_get port main port "$DEFAULT_PORT"
	config_get listen main listen "$DEFAULT_LISTEN"
	config_get data_dir main data_dir "$DEFAULT_DATA_DIR"
	addr=$(listen_addr "$listen") || {
		echo "the lan interface has no address yet" >&2
		return 1
	}
	{
		echo "# Written by /etc/init.d/$NAME from /etc/config/$NAME at each start: change the settings there, or in $PAGE."
		toml_set "$LISTEN_KEY" "$addr:$port" &&
			toml_set "$STORE_KEY" "$data_dir/$STORE_FILE" &&
			toml_set status.listen "$STATUS_LISTEN" &&
			toml_options main "$OPTIONS_MAP" &&
			render_more
	} >"$1"
}

# check_options <file>: the binary's --check on that file, with the secrets.
check_options() {
	with_secrets "$BIN" --options "$1" --check 2>&1
}

# check: what the next start would refuse, in the binary's own words (or
# what it would run on, then OK). LuCI runs it before it restarts the
# service. Writes nothing but a file in /tmp, removed again.
check() {
	local file output ok=0
	config_load "$NAME"
	if output=$(blocked); then
		echo "$output"
		return 1
	fi
	file=$(mktemp "/tmp/$NAME.XXXXXX") || return 1
	if ! output=$(render_options "$file" 2>&1); then
		rm -f "$file"
		echo "$output"
		return 1
	fi
	output=$(check_options "$file") || ok=1
	rm -f "$file"
	echo "$output"
	return $ok
}

# refuse <why>: logged, and kept for LuCI.
refuse() {
	log "not started: $1"
	printf '%s\n' "$1" >"$CHECK_FILE"
}

# Whether procd runs the service now. Not procd_running, which would reset
# the service description start_service is in the middle of.
running_now() {
	ubus call service list "{\"name\":\"$NAME\"}" 2>/dev/null |
		jsonfilter -q -e "@['$NAME'].instances.*.running" | grep -q true
}

start_service() {
	local enabled listen data_dir output secret keep_running=0

	config_load "$NAME"
	config_get_bool enabled main enabled 1
	[ "$enabled" = 1 ] || return 0
	config_get listen main listen "$DEFAULT_LISTEN"
	config_get data_dir main data_dir "$DEFAULT_DATA_DIR"
	# A reload (a save, `uci commit`'s trigger, the LAN coming up) of a
	# running service keeps it on its last good settings if the new ones
	# are refused; a start (a restart's included) doesn't start it.
	[ "${RELOADING:-0}" = 1 ] && running_now && keep_running=1

	rm -f "$BLOCKED_FILE"
	if output=$(blocked); then
		refuse "$output"
		echo "$output" >"$BLOCKED_FILE"
		return 1
	fi
	if [ "$listen" = lan ] && ! listen_addr lan >/dev/null; then
		log "the lan interface has no address yet; waiting for it"
		return 0
	fi
	secrets || return 1
	prepare_data_dir "$data_dir" || return 1

	mkdir -p "${OPTIONS%/*}"
	if output=$(render_options "$OPTIONS.new" 2>&1) && output=$(check_options "$OPTIONS.new"); then
		chmod 644 "$OPTIONS.new"
		mv "$OPTIONS.new" "$OPTIONS"
		rm -f "$CHECK_FILE"
	else
		rm -f "$OPTIONS.new"
		refuse "$output"
		if [ "$keep_running" = 1 ] && [ -s "$OPTIONS" ]; then
			log "keeps running on its last good settings"
		else
			return 1
		fi
	fi

	procd_open_instance "$NAME"
	procd_set_param command "$BIN" --options "$OPTIONS"
	set --
	for secret in $SECRETS; do
		[ -s "$SECRETS_DIR/$secret" ] && set -- "$@" "$secret=$(cat "$SECRETS_DIR/$secret")"
	done
	[ $# -gt 0 ] && procd_set_param env "$@"
	procd_set_param user "$RUN_AS"
	# The build, kept with the instance so LuCI reports the one running,
	# not only the one installed, and the options file's hash: a new build
	# or new settings change it, so procd restarts the instance onto them.
	procd_set_param data \
		version="$(sed -n 's/^version=//p' "$VERSION_FILE" 2>/dev/null)" \
		commit="$(sed -n 's/^commit=//p' "$VERSION_FILE" 2>/dev/null)" \
		options="$(md5sum "$OPTIONS" | cut -d' ' -f1)"
	procd_set_param respawn 3600 5 5
	procd_set_param stdout 1
	procd_set_param stderr 1
	procd_close_instance
}

reload_service() {
	RELOADING=1
	start
}
