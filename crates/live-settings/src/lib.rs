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
//! Values resolve the command line over the environment over stored over
//! default, per key: an invalid value falls back to that setting's default
//! and nothing else. A setting that can't be saved (its `sources` leave out
//! the database) is read from the command line and the environment alone,
//! before any store exists if need be ([`Setting::require`]). [`cli`] turns
//! the declared settings into command-line options, with help.
//!
//! The crate knows nothing about either process or its database. Each
//! plugs in its own [`SettingsStore`].

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;
mod registry;
mod section;
mod setting;
mod store;
mod value;

pub use registry::{
    read_sync, read_sync_with_env, BootError, BootReport, BuildError, Changes, Registry,
    RegistryBuilder, SaveError, SaveReport, SettingView,
};
pub use section::{BootPolicy, FieldError, Live, Reloadable, Section, Warning};
pub use setting::{
    cli_flag, parsed_default, range, AnySetting, Applies, Bounds, Check, Env, Problem, Setting,
    SettingSource, Snapshot, Source, Sources,
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
///         env: "ENGINE_PAYMENT_REORG_CHECK_DEPTH",
///         default: 20,
///         check: range(1, 10_000),
///         description: "How many recent blocks are checked again on every scan for a chain reorganisation.",
///         example: "20",
///     },
///     WORKER_THREADS: usize {
///         key: "server.worker_threads",
///         env: "ENGINE_SERVER_WORKER_THREADS",
///         default: 2,
///         check: range(1, 256),
///         description: "Threads serving requests.",
///         applies: Restart,
///     },
///     SIGNUP_NOTE: String {
///         key: "signup.note",
///         env: "MONOKULO_SIGNUP_NOTE",
///         default: "Invite only".to_string(),
///         check: |v: &String| if v.len() <= 200 { Ok(()) } else { Err("Keep it under 200 characters.".to_string()) },
///         description: "Shown on the signup page.",
///     },
/// }
/// assert_eq!(ALL.len(), 3);
/// assert_eq!(REORG_CHECK_DEPTH.default_value(), 20);
/// ```
///
/// Fields may come in any order. `key`, `env`, `default` and `description`
/// are required. `default` is an expression of the setting's type,
/// evaluated whenever the default is needed. `check` is either
/// `range(min, max)` (whole numbers only; the admin page gets the bounds
/// too) or a function or closure `fn(&T) -> Result<(), String>`. `example`
/// is a raw value, checked by `Registry::build`. `applies` is `Live` (the
/// default) or `Restart`. `sources: [Cli, Env]` limits where the value may
/// come from (all of `Cli`, `Env` and `Database` otherwise): a setting that
/// can't be saved is never stored, refused by a save and shown locked, for
/// what is needed before the store opens or must never be kept in it.
/// `required: true` makes such a setting have no usable default: unset, the
/// process can't start ([`Setting::require`]), and `default` is only a
/// placeholder.
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
                {key=[] env=[] default=[] description=[]
                check=[::core::option::Option::None]
                bounds=[::core::option::Option::None]
                example=[::core::option::Option::None]
                applies=[$crate::Applies::Live]
                sources=[$crate::Sources::ALL]
                required=[false]}
                $($body)*
            );
        )+
        /// Every setting declared in this block.
        pub const ALL: &[&dyn $crate::AnySetting] = &[$(&$name),+];
    };

    (@field $attrs:tt $name:ident $ty:tt {key=$k0:tt env=$e:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r:tt}
        key: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=[$v] env=$e default=$d description=$ds check=$c bounds=$b example=$x applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e0:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r:tt}
        env: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=[$v] default=$d description=$ds check=$c bounds=$b example=$x applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d0:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r:tt}
        default: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=[$v] description=$ds check=$c bounds=$b example=$x applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds0:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r:tt}
        description: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=[$v] check=$c bounds=$b example=$x applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    // `range(min, max)` must come before the general `check` rule.
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b0:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r:tt}
        check: range($min:expr, $max:expr) $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=$ds check=$c bounds=[$crate::range($min, $max)] example=$x applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds:tt check=$c0:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r:tt}
        check: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=$ds check=[::core::option::Option::Some($v)] bounds=$b example=$x applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x0:tt applies=$a:tt sources=$o:tt required=$r:tt}
        example: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=$ds check=$c bounds=$b example=[::core::option::Option::Some($v)] applies=$a sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a0:tt sources=$o:tt required=$r:tt}
        applies: $v:ident $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=$ds check=$c bounds=$b example=$x applies=[$crate::Applies::$v] sources=$o required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o0:tt required=$r:tt}
        sources: [$($s:ident),* $(,)?] $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=$ds check=$c bounds=$b example=$x applies=$a sources=[$crate::Sources::of(&[$($crate::Source::$s),*])] required=$r} $($($rest)*)?);
    };
    (@field $attrs:tt $name:ident $ty:tt {key=$k:tt env=$e:tt default=$d:tt description=$ds:tt check=$c:tt bounds=$b:tt example=$x:tt applies=$a:tt sources=$o:tt required=$r0:tt}
        required: $v:expr $(, $($rest:tt)*)?) => {
        $crate::settings!(@field $attrs $name $ty {key=$k env=$e default=$d description=$ds check=$c bounds=$b example=$x applies=$a sources=$o required=[$v]} $($($rest)*)?);
    };

    // Every field consumed.
    (@field [$($attr:tt)*] $name:ident [$ty:ty]
        {key=[$key:expr] env=[$env:expr] default=[$default:expr] description=[$description:expr]
        check=[$check:expr] bounds=[$bounds:expr] example=[$example:expr] applies=[$applies:expr]
        sources=[$sources:expr] required=[$required:expr]}) => {
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
        };
    };
}

#[cfg(test)]
mod tests;
