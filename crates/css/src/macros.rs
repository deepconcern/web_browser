//! Internal macros.

/// Declares a C-like enum of CSS keywords with `from_ident`, `as_str` and a
/// cursor-based `parse`.
macro_rules! keyword_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $css:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $(
                #[doc = concat!("`", $css, "`")]
                $variant,
            )+
        }

        impl $name {
            /// Parses the keyword (ASCII case-insensitive).
            pub fn from_ident(ident: &str) -> Option<Self> {
                $(
                    if ident.eq_ignore_ascii_case($css) {
                        return Some(Self::$variant);
                    }
                )+
                None
            }

            /// The keyword as written in CSS.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $css,)+
                }
            }

            #[allow(dead_code)]
            pub(crate) fn parse(c: &mut $crate::cursor::Cursor<'_>) -> Option<Self> {
                c.attempt(|c| c.ident().and_then(Self::from_ident))
            }
        }
    };
}
