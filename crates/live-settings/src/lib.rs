//! Typed settings that a running process can apply without a restart.
//!
//! Before this crate, each process read its settings once at boot, by key,
//! as whatever type the caller asked for. Saving a setting on the admin
//! page persisted it and changed nothing, because nothing knew which loops
//! and clients depended on it. This crate fixes that by making the
//! dependency explicit:
//!
//! - A [`Setting<T>`] fixes its value type at declaration, so every reader
//!   gets the same type, and parsing and validation live on the type
//!   ([`SettingValue`]).
//! - A [`Section`] groups the settings one runtime piece depends on into a
//!   plain struct, and can reject combinations of values.
//! - Readers hold a [`Live<S>`] of their section. There is no string-keyed
//!   getter, so a boot-only read that never reloads can't be written by
//!   accident.
//! - A [`Reloadable`] is runtime state built from a section (node clients,
//!   a listener). A save prepares the new state first and only installs it
//!   once every change is valid and stored.
//! - The [`Registry`] owns all of it: it boots every section, saves
//!   changes in one validated, all-or-nothing step, and describes every
//!   setting for the admin page.
//!
//! Values resolve the environment (secrets only) over the command line over
//! stored over default, per key. Stored means the options file
//! ([`OptionsFile`]) for configuration, or the database for a runtime
//! switch: a [`LayeredStore`] keeps each key in its place. Any invalid
//! value stops [`Registry::build`] with every problem named, so a process
//! never starts on a value it would have to guess about. A secret is read
//! from the environment alone, before any store exists if need be
//! ([`Setting::require`]). [`cli`] turns the declared settings into
//! command-line options, with help, plus `--options` and `--init`.
//!
//! The crate knows nothing about either process or its database. Each
//! plugs in its own [`SettingsStore`].

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;
mod options;
pub mod paths;
mod registry;
mod section;
mod setting;
mod store;
mod value;

pub use options::{
    render_init, render_init_nested, write_init, FileInfo, LayeredStore, OptionsFile,
};
pub use registry::{
    read_sync, read_sync_with_env, BootError, BootReport, BuildError, Changes, Registry,
    RegistryBuilder, SaveError, SaveReport, SettingView,
};
pub use section::{BootPolicy, FieldError, Live, Reloadable, Section, Warning};
pub use setting::{
    cli_flag, outside_names, parsed_default, range, AnySetting, Applies, Bounds, Check, Env,
    Problem, Setting, SettingSource, Snapshot, Source, Sources,
};
pub use store::{MemoryStore, SettingsStore, StoreError};
pub use value::{BindAddr, CommaList, HttpUrl, Json, Secret, SettingKind, SettingValue, MASK};

/// Re-exported so implementations of [`Reloadable`] don't need their own
/// dependency on it.
pub use async_trait::async_trait;

/// Declares settings as `pub const` items, plus `pub const ALL`, every
/// setting in the block, for `Registry::builder`.
///
/// ```
/// live_settings::settings! {
///     /// Checked again on every scan.
///     REORG_CHECK_DEPTH: u64 {
///         key: "payment.reorg_check_depth",
///         default: 20,
///         check: range(1, 10_000),
///         description: "How many recent blocks are checked again on every scan for a chain reorganisation.",
///         example: "20",
///     },
///     WORKER_THREADS: usize {
///         key: "server.worker_threads",
///         default: 2,
///         check: range(1, 256),
///         description: "Threads serving requests.",
///         applies: Restart,
///     },
///     SIGNUP_NOTE: String {
///         key: "signup.note",
///         default: "Invite only".to_string(),
///         check: |v: &String| if v.len() <= 200 { Ok(()) } else { Err("Keep it under 200 characters.".to_string()) },
///         description: "Shown on the signup page.",
///     },
///     API_KEY: live_settings::Secret {
///         key: "api.key",
///         env: "APP_API_KEY",
///         default: live_settings::Secret::default(),
///         description: "The key for the upstream API.",
///         sources: [Env],
///         required: true,
///     },
/// }
/// assert_eq!(ALL.len(), 4);
/// assert_eq!(REORG_CHECK_DEPTH.default_value(), 20);
/// ```
///
/// Fields may come in any order. `key`, `default` and `description` are
/// required; `env` (the variable's name) only for a secret. `default` is an expression of the setting's type, evaluated
/// whenever the default is needed. `check` is either `range(min, max)`
/// (whole numbers only; the admin page gets the bounds too) or a function or
/// closure `fn(&T) -> Result<(), String>`. `example` is a raw value, checked
/// by `Registry::build`. `applies` is `Live` (the default) or `Restart`.
///
/// `sources` says where the value may come from: the options file and the
/// command line unless it says otherwise (`[Toml, Cli]`). A secret comes
/// from its environment variable alone (`sources: [Env]`, with `env`), and a
/// runtime switch kept by the admin page from the database alone
/// (`sources: [Database]`). `required: true` makes a setting with no usable
/// default: unset, the process can't start ([`Setting::require`]).
/// `editable: false` keeps the admin page from changing it, though it may be
/// in the options file (where the database is).
///
/// The items are `const`, as the plan asked: a `Setting` holds only
/// `&'static str`s and function pointers, so `const` works, and it lets a
/// section list its keys as `&[&A, &B]` in a function body (constants are
/// promoted to `'static`; statics would need a separate `static` list).
#[macro_export]
macro_rules! settings {
    ( $( $(#[$attr:meta])* $name:ident : $ty:ty { $($body:tt)* } ),+ $(,)? ) => {
        $(
            $crate::settings!(@field [$(#[$attr])*] $name [$ty]
                {key=[] env=[""] default=[] description=[]
                check=[::core::option::Option::None]
                bounds=[::core::option::Option::None]
                example=[::core::option::Option::None]
                applies=[$crate::Applies::Live]
                sources=[$crate::Sources::CONFIG]
                required=[false]
                editable=[true]}
                $($body)*
            );
        )+
        /// Every setting declared in this block.
        pub const ALL: &[&dyn $crate::AnySetting] = &[$(&$name),+];
    };

    (@field $attrs:tt $name:ident $ty:tt {key=$ke0x:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        key: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=[$v] env=$en1 default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1x:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        env: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=[$v] default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2x:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        default: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=[$v] description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3x:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        description: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=[$v] check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    // `range(min, max)` must come before the general `check` rule.
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5x:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        check: range($min:expr, $max:expr) $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=$ch4 bounds=[$crate::range($min, $max)] example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4x:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        check: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=[::core::option::Option::Some($v)] bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6x:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        example: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=[::core::option::Option::Some($v)] applies=$ap7 sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7x:tt sources=$so8:tt required=$re9:tt editable=$ed10:tt}
        applies: $v:ident $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=[$crate::Applies::$v] sources=$so8 required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8x:tt required=$re9:tt editable=$ed10:tt}
        sources: [$($s:ident),* $(,)?] $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=[$crate::Sources::of(&[$($crate::Source::$s),*])] required=$re9 editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9x:tt editable=$ed10:tt}
        required: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=[$v] editable=$ed10} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$ke0:tt env=$en1:tt default=$de2:tt description=$de3:tt check=$ch4:tt bounds=$bo5:tt example=$ex6:tt applies=$ap7:tt sources=$so8:tt required=$re9:tt editable=$ed10x:tt}
        editable: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$ke0 env=$en1 default=$de2 description=$de3 check=$ch4 bounds=$bo5 example=$ex6 applies=$ap7 sources=$so8 required=$re9 editable=[$v]} $($($rest)*)?);
    };

    // Every field consumed.
    (@field [$($attr:tt)*] $name:ident [$ty:ty]
        {key=[$key:expr] env=[$env:expr] default=[$default:expr] description=[$description:expr]
        check=[$check:expr] bounds=[$bounds:expr] example=[$example:expr] applies=[$applies:expr]
        sources=[$sources:expr] required=[$required:expr] editable=[$editable:expr]}) => {
        $($attr)*
        pub const $name: $crate::Setting<$ty> = $crate::Setting {
            key: $key,
            env_var: $env,
            default: || $default,
            check: $check,
            bounds: $bounds,
            description: $description,
            example: $example,
            applies: $applies,
            sources: $sources,
            required: $required,
            editable: $editable,
        };
    };
}

#[cfg(test)]
mod tests;
