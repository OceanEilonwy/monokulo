# Configuration

monokulo keeps its settings in an options file, TOML, at
`~/.config/monokulo/monokulo.toml` (`$XDG_CONFIG_HOME` if set; the working
directory if that can't be used), or wherever `--options <PATH>` says. The
engine inside it keeps its settings in the same file, under `[engine.*]`
tables (`[engine.payment]` holds its `payment.…`); a standalone engine keeps
them in its own `engine.toml`. `monokulo --init` (and `monokulo-engine
--init`) write one with every setting, its default commented out and what
it does, and print where. The admin settings page edits the file in place,
keeping your comments, and applies the change at once; after editing it by
hand, press Reload options file on that page. A save is refused if the file
changed on disk since it was read, and every setting the file holds is
locked on the page if the process can't write it.

Every setting can also be given as a command-line option: the setting's
key with `-` for `.` and `_` (`payment.reorg_check_depth` is
`--payment-reorg-check-depth`), and the embedded engine's with `--engine-`
in front (`--engine-payment-reorg-check-depth`). An option wins over the
file, and the admin page shows that setting locked. `monokulo --help` and
`monokulo-engine --help` list every option with its default and what it
does; `-h` gives the short version.

Secrets are only ever taken from the environment, never an option or the
file, since every user of the machine can read a process's arguments:
`MONOKULO_ENCRYPTION_KEY` and `MONOKULO_LOGGING_OTLP_HEADERS` for monokulo;
with a remote engine, `MONOKULO_ENGINE_TOKEN` too, and `ENGINE_TOKEN` and
`ENGINE_LOGGING_OTLP_HEADERS` for the engine. `--help` lists them after the
options. Two runtime switches, `abuse.under_attack` and
`logging.dev_mode_until`, are kept in each database instead, since they
are flipped while running rather than configured. Each field on the admin
page has a chip saying where its value comes from.

An invalid value anywhere - the file (named by line), an option, the
environment or the database - stops the process at start with every
problem listed, so a value an upgrade no longer accepts is fixed, not
silently replaced. So does a setting that only the other engine mode uses
(an engine URL or token with the engine inside monokulo, the standalone
engine's own server and logging settings under `[engine.*]`, or
`[engine.*]` tables with a remote engine), and a missing encryption key.
