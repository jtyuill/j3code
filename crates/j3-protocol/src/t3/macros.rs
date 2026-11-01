//! Serde helpers for t3 contract shapes that derive can't express directly.

/// A tagged union whose unknown tags decode to `Unknown(Value)` and re-encode
/// unchanged. Known tags decode strictly: a known tag with a bad body is an
/// error, never `Unknown` (t3: "A known kind must decode in full").
///
/// ```ignore
/// open_union! {
///     /// docs
///     pub enum Item {
///         tag = "kind";                       // or: tag = "type", content = "payload";
///         newtype { "snapshot" => Snapshot(SnapshotItem), }
///         unit { "synchronized" => Synchronized, }
///     }
/// }
/// ```
macro_rules! open_union {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            tag = $tag:literal $(, content = $content:literal)?;
            newtype { $( $(#[$vmeta:meta])* $value:literal => $variant:ident($ty:ty) ),* $(,)? }
            unit { $( $(#[$umeta:meta])* $uvalue:literal => $uvariant:ident ),* $(,)? }
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        #[allow(clippy::large_enum_variant)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant($ty), )*
            $( $(#[$umeta])* $uvariant, )*
            /// A tag this build does not know, kept verbatim.
            Unknown(::serde_json::Value),
        }

        impl $name {
            /// Every tag value this build decodes into a typed variant.
            pub const KNOWN_TAGS: &'static [&'static str] = &[$($value,)* $($uvalue,)*];

            /// The tag value, if known or present in the unknown body.
            pub fn tag(&self) -> Option<&str> {
                match self {
                    $( Self::$variant(_) => Some($value), )*
                    $( Self::$uvariant => Some($uvalue), )*
                    Self::Unknown(value) => value.get($tag).and_then(::serde_json::Value::as_str),
                }
            }
        }

        const _: () = {
            #[derive(::serde::Serialize)]
            #[serde(tag = $tag $(, content = $content)?)]
            #[allow(clippy::enum_variant_names)]
            enum Ser<'a> {
                $( #[serde(rename = $value)] $variant(&'a $ty), )*
                $( #[serde(rename = $uvalue)] $uvariant, )*
                #[serde(skip)]
                #[allow(dead_code)]
                __Lifetime(::core::marker::PhantomData<&'a ()>),
            }

            #[derive(::serde::Deserialize)]
            #[serde(tag = $tag $(, content = $content)?)]
            #[allow(clippy::enum_variant_names, clippy::large_enum_variant)]
            enum De {
                $( #[serde(rename = $value)] $variant($ty), )*
                $( #[serde(rename = $uvalue)] $uvariant, )*
            }

            impl ::serde::Serialize for $name {
                fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                    match self {
                        $( Self::$variant(inner) => Ser::$variant(inner).serialize(serializer), )*
                        $( Self::$uvariant => Ser::$uvariant.serialize(serializer), )*
                        Self::Unknown(value) => value.serialize(serializer),
                    }
                }
            }

            impl<'de> ::serde::Deserialize<'de> for $name {
                fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                    let value = ::serde_json::Value::deserialize(deserializer)?;
                    let known = value
                        .get($tag)
                        .and_then(::serde_json::Value::as_str)
                        .is_some_and(|tag| Self::KNOWN_TAGS.contains(&tag));
                    if !known {
                        return Ok(Self::Unknown(value));
                    }
                    let de = De::deserialize(value).map_err(<D::Error as ::serde::de::Error>::custom)?;
                    Ok(match de {
                        $( De::$variant(inner) => Self::$variant(inner), )*
                        $( De::$uvariant => Self::$uvariant, )*
                    })
                }
            }
        };
    };
}

/// A JSON literal (`Schema.Literal(1)`, `Schema.Literal(true)`) as a
/// zero-sized type that encodes to that literal and accepts only it.
macro_rules! json_literal {
    ($(#[$meta:meta])* $vis:vis struct $name:ident = $kind:ident($value:literal);) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
        $vis struct $name;

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                json_literal!(@ser $kind serializer $value)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = ::serde_json::Value::deserialize(deserializer)?;
                if value == ::serde_json::json!($value) {
                    Ok($name)
                } else {
                    Err(<D::Error as ::serde::de::Error>::custom(format_args!(
                        "expected literal {}, got {}",
                        stringify!($value),
                        value
                    )))
                }
            }
        }
    };
    (@ser int $serializer:ident $value:literal) => { $serializer.serialize_u64($value) };
    (@ser bool $serializer:ident $value:literal) => { $serializer.serialize_bool($value) };
    (@ser str $serializer:ident $value:literal) => { $serializer.serialize_str($value) };
}

pub(crate) use json_literal;
