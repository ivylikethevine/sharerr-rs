//! Shared machinery for two recurring shapes: the fieldless, string-keyed
//! enums scattered across this crate and `sharerr-store` ([`str_enum!`]),
//! and the flat lists of dotted string constants each written twice — once
//! as a `pub const`, once again in an `ALL` slice a caller must remember to
//! update ([`config_paths!`], [`secret_keys!`]).

/// Generate `ALL`, `as_str`, and `parse` for a fieldless enum whose wire or
/// storage representation is exactly what `as_str` returns.
///
/// Nine enums across `sharerr-core` and `sharerr-store` hand-wrote this
/// trio identically apart from names, each carrying its own copy of
/// "derived from `as_str` so the two cannot drift" — the tell that the
/// mechanism, not the comment, should be shared. `ALL` is also what the
/// settings UI and `vault set`/`vault list` enumerate, so a hand-written
/// `parse` that quietly diverges from `as_str` is exactly the trap
/// `CLAUDE.md` documents for the config-path constants.
///
/// Three forms:
///
/// - `str_enum!(Type { Variant => "str", ... });` — `parse` returns
///   `Option<Self>`, `None` for anything unrecognised. The default; use
///   this unless the type needs one of the two below.
/// - `str_enum!(Type { Variant => "str", ... }, "why parse matters here");`
///   — same as above, plus an extra paragraph on `parse`'s generated doc
///   comment for a type whose decode failures carry a consequence worth
///   spelling out (see [`sharerr_core::model::MediaSource`] parse's own
///   invocation for the canonical example).
/// - `str_enum!(Type { Variant => "str", ... }, lenient = Default, "why");`
///   — `parse` returns `Self` directly, falling back to `Self::Default` for
///   anything unrecognised. `"why"` is mandatory here, not optional: a
///   widening default is a correctness decision, and the reasoning for
///   picking that specific direction to widen in belongs beside the code
///   that acts on it, not lost in the commit that introduced this macro.
#[macro_export]
macro_rules! str_enum {
    ($ty:ty { $($variant:ident => $str:literal),+ $(,)? }) => {
        impl $ty {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The wire/storage spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $str,)+
                }
            }

            /// Inverse of [`Self::as_str`], derived from it so the two
            /// cannot drift.
            pub fn parse(value: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == value)
            }
        }
    };
    ($ty:ty { $($variant:ident => $str:literal),+ $(,)? }, $reason:literal) => {
        impl $ty {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The wire/storage spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $str,)+
                }
            }

            #[doc = concat!(
                "Inverse of [`Self::as_str`], derived from it so the two cannot drift.\n\n",
                $reason
            )]
            pub fn parse(value: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == value)
            }
        }
    };
    ($ty:ty { $($variant:ident => $str:literal),+ $(,)? }, lenient = $default:ident, $reason:literal) => {
        impl $ty {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The wire/storage spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $str,)+
                }
            }

            #[doc = concat!(
                "Inverse of [`Self::as_str`], derived from it so the two cannot drift.\n\n",
                $reason
            )]
            pub fn parse(value: &str) -> Self {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|v| v.as_str() == value)
                    .unwrap_or(Self::$default)
            }
        }
    };
}

/// Declare a flat list of dotted config-path `pub const`s and generate
/// `ALL` from the same list, so a path cannot be added here and forgotten
/// in `ALL` — `ALL` is what the settings UI enumerates, and a path missing
/// from it is a field the UI silently will not manage.
///
/// Each entry keeps its own doc comment exactly as if it were a plain
/// `pub const` declaration — a `///` line directly above an entry attaches
/// to the constant `config_paths!` generates for it, the same as it would
/// without the macro.
#[macro_export]
macro_rules! config_paths {
    ($($(#[$doc:meta])* $name:ident = $value:literal;)+) => {
        $(
            $(#[$doc])*
            pub const $name: &str = $value;
        )+

        /// Every path the web UI writes back to `sharerr.toml`, generated
        /// from the constants above — see the module doc for why a
        /// hand-maintained second copy of this list was the trap.
        pub const ALL: &[&str] = &[$($name),+];
    };
}

/// [`config_paths!`], split into two blocks instead of one: `editable`
/// constants are what feed `ALL` (the web UI's editable fields and what
/// `sharerr vault set`/`vault list` accept), `generated` constants are
/// deliberately absent from it — minted on first use rather than typed by
/// an operator, so "editing" one would silently break something (a
/// friendship pinned to an old signing key, for example) rather than
/// update a setting.
///
/// The split is the point: which block a key is declared in is a marker a
/// reader sees at the declaration, not an omission they have to notice by
/// the key's *absence* from a separately maintained `ALL`.
#[macro_export]
macro_rules! secret_keys {
    (
        editable {
            $($(#[$edoc:meta])* $ename:ident = $evalue:literal;)+
        }
        generated {
            $($(#[$gdoc:meta])* $gname:ident = $gvalue:literal;)*
        }
    ) => {
        $(
            $(#[$edoc])*
            pub const $ename: &str = $evalue;
        )+
        $(
            $(#[$gdoc])*
            pub const $gname: &str = $gvalue;
        )*

        /// Every key sharerr treats as operator-editable: what `vault set`
        /// warns outside of, and what the web UI offers as editable fields.
        /// Generated keys are deliberately not here — see their own doc
        /// comments in the `generated` block above.
        pub const ALL: &[&str] = &[$($ename),+];
    };
}
