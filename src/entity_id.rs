//! Typed Wikibase entity IDs: items (`Q42`), properties (`P31`), lexemes
//! (`L7`) with their forms (`L7-F2`) and senses (`L7-S1`), MediaInfo entities
//! (`M123`, Commons files) and entity schemas (`E10`).
//!
//! Each kind has its own [`Copy`] type, so a property can never be passed where
//! an item is expected; [`EntityId`] holds any of them. All of them display in
//! their canonical form (`Q42`), which is therefore always safe to splice into
//! a SPARQL query or URL.
//!
//! There are three ways in, from strict to forgiving:
//!
//! - [`FromStr`] (`"Q42".parse()`) accepts the canonical form only: uppercase
//!   prefix, ASCII digits, no leading zero, no surrounding whitespace.
//! - `from_uri` accepts an entity URI as found in SPARQL results or RDF
//!   (`http://www.wikidata.org/entity/Q42`, any host, http or https) and
//!   nothing else.
//! - `parse_lenient` accepts what people type or paste: surrounding whitespace,
//!   a lowercase prefix, leading zeros, entity URIs, page URLs
//!   (`https://www.wikidata.org/wiki/Property:P31`), `wd:Q42`, and — for the
//!   kind-specific types only — a bare number (`"42"` as an [`ItemId`]).
//!
//! The number `0` is never a valid ID; `ItemId::new(0)` is allowed for callers
//! that use it as a "none" marker in numeric storage, but no parser returns it.

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The concept URI prefix of Wikidata entities.
pub const WIKIDATA_ENTITY_PREFIX: &str = "http://www.wikidata.org/entity/";

/// The concept URI prefix of Wikimedia Commons entities (MediaInfo).
pub const COMMONS_ENTITY_PREFIX: &str = "https://commons.wikimedia.org/entity/";

/// Failure modes of entity ID parsing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EntityIdError {
    /// The text is not an entity ID (or URI) of any kind.
    #[error("'{0}' is not a valid entity ID")]
    Invalid(String),

    /// The text is a valid ID, but of another kind than the one asked for
    /// (`P31` where an item was expected).
    #[error("'{input}' is not a {expected} ID")]
    WrongKind { input: String, expected: EntityKind },
}

/// The kinds of Wikibase entities.
///
/// The order of the variants is the order [`EntityId`]s sort in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntityKind {
    Item,
    Property,
    Lexeme,
    Form,
    Sense,
    MediaInfo,
    EntitySchema,
}

impl EntityKind {
    /// The letter that marks this kind in an ID: `Q`, `P`, `L`, `F`, `S`, `M`,
    /// `E`. For forms and senses it is the letter after the dash (`L7-F2`).
    pub const fn prefix(self) -> char {
        match self {
            Self::Item => 'Q',
            Self::Property => 'P',
            Self::Lexeme => 'L',
            Self::Form => 'F',
            Self::Sense => 'S',
            Self::MediaInfo => 'M',
            Self::EntitySchema => 'E',
        }
    }

    /// The kind of top-level entity whose IDs start with `prefix` (uppercase
    /// only). Forms and senses have no prefix of their own.
    pub const fn from_prefix(prefix: char) -> Option<Self> {
        match prefix {
            'Q' => Some(Self::Item),
            'P' => Some(Self::Property),
            'L' => Some(Self::Lexeme),
            'M' => Some(Self::MediaInfo),
            'E' => Some(Self::EntitySchema),
            _ => None,
        }
    }

    /// The MediaWiki namespace the entity's page lives in on Wikidata
    /// (MediaInfo: on Commons, where it is the file page). Forms and senses
    /// live on their lexeme's page.
    pub const fn namespace(self) -> i64 {
        match self {
            Self::Item => 0,
            Self::Property => 120,
            Self::Lexeme | Self::Form | Self::Sense => 146,
            Self::MediaInfo => 6,
            Self::EntitySchema => 640,
        }
    }

    /// The entity type as Wikibase names it in JSON and API parameters.
    pub const fn entity_type(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Property => "property",
            Self::Lexeme => "lexeme",
            Self::Form => "form",
            Self::Sense => "sense",
            Self::MediaInfo => "mediainfo",
            Self::EntitySchema => "entity-schema",
        }
    }

    /// The kind with the given Wikibase entity type name; the inverse of
    /// [`Self::entity_type`].
    pub fn from_entity_type(entity_type: &str) -> Option<Self> {
        ALL_KINDS
            .iter()
            .copied()
            .find(|kind| kind.entity_type() == entity_type)
    }

    /// The namespace prefix of the entity's page title, if it has one (items
    /// live in the main namespace, MediaInfo has no page title of its own).
    const fn page_prefix(self) -> Option<&'static str> {
        match self {
            Self::Property => Some("Property:"),
            Self::Lexeme | Self::Form | Self::Sense => Some("Lexeme:"),
            Self::EntitySchema => Some("EntitySchema:"),
            Self::Item | Self::MediaInfo => None,
        }
    }
}

const ALL_KINDS: [EntityKind; 7] = [
    EntityKind::Item,
    EntityKind::Property,
    EntityKind::Lexeme,
    EntityKind::Form,
    EntityKind::Sense,
    EntityKind::MediaInfo,
    EntityKind::EntitySchema,
];

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.entity_type())
    }
}

macro_rules! numeric_id {
    ($(#[$doc:meta])* $name:ident, $kind:ident, $variant:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            /// The kind of entity this type identifies.
            pub const KIND: EntityKind = EntityKind::$kind;

            /// The ID with the given number. Not validated: `0` is accepted
            /// for callers that use it as a "none" marker.
            pub const fn new(number: u64) -> Self {
                Self(number)
            }

            /// The number of the ID (`42` for `Q42`).
            pub const fn get(self) -> u64 {
                self.0
            }

            /// Parses what people type or paste; see the [module
            /// documentation](self). A bare number is accepted too.
            pub fn parse_lenient(s: &str) -> Result<Self, EntityIdError> {
                let trimmed = s.trim();
                if let Some(number) = parse_number(trimmed, true) {
                    return Ok(Self(number));
                }
                EntityId::parse_lenient(trimmed)
                    .map_err(|_| invalid(s))
                    .and_then(|id| Self::try_from(id).map_err(|_| wrong_kind(s, Self::KIND)))
            }

            /// Parses an entity URI (`http://www.wikidata.org/entity/Q42`).
            pub fn from_uri(uri: &str) -> Result<Self, EntityIdError> {
                EntityId::from_uri(uri)
                    .and_then(|id| Self::try_from(id).map_err(|_| wrong_kind(uri, Self::KIND)))
            }

            /// The concept URI (`http://www.wikidata.org/entity/Q42`).
            pub fn uri(self) -> String {
                EntityId::from(self).uri()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", Self::KIND.prefix(), self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, f)
            }
        }

        impl FromStr for $name {
            type Err = EntityIdError;

            /// Parses the canonical form (`Q42`) only.
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.strip_prefix(Self::KIND.prefix())
                    .and_then(|digits| parse_number(digits, false))
                    .map(Self)
                    .ok_or_else(|| match s.parse::<EntityId>() {
                        Ok(_) => wrong_kind(s, Self::KIND),
                        Err(_) => invalid(s),
                    })
            }
        }

        impl From<$name> for EntityId {
            fn from(id: $name) -> Self {
                EntityId::$variant(id)
            }
        }

        impl TryFrom<EntityId> for $name {
            type Error = EntityIdError;

            fn try_from(id: EntityId) -> Result<Self, Self::Error> {
                match id {
                    EntityId::$variant(id) => Ok(id),
                    other => Err(wrong_kind(&other.to_string(), Self::KIND)),
                }
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                other.parse::<Self>().is_ok_and(|id| id == *self)
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self == *other
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            /// Accepts a string (leniently, as [`Self::parse_lenient`]) or a
            /// positive number.
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserializer.deserialize_any(IdVisitor::<Self>::new())
            }
        }

        impl FromIdInput for $name {
            fn from_str_input(s: &str) -> Result<Self, EntityIdError> {
                Self::parse_lenient(s)
            }

            fn from_number(number: u64) -> Option<Self> {
                (number > 0).then_some(Self(number))
            }
        }

        #[cfg(feature = "database")]
        impl From<$name> for crate::mysql_async::Value {
            /// Binds the number, for numeric ID columns.
            fn from(id: $name) -> Self {
                crate::mysql_async::Value::UInt(id.0)
            }
        }
    };
}

numeric_id!(
    /// An item ID, `Q42`.
    ItemId,
    Item,
    Item
);
numeric_id!(
    /// A property ID, `P31`.
    PropertyId,
    Property,
    Property
);
numeric_id!(
    /// A lexeme ID, `L7`.
    LexemeId,
    Lexeme,
    Lexeme
);
numeric_id!(
    /// A MediaInfo ID, `M123`: the structured data of a Commons file. The
    /// number is the page ID of the file page.
    MediaInfoId,
    MediaInfo,
    MediaInfo
);
numeric_id!(
    /// An entity schema ID, `E10`.
    EntitySchemaId,
    EntitySchema,
    EntitySchema
);

impl PropertyId {
    /// Parses a property predicate URI from Wikidata's RDF, as bound by `wdt:`,
    /// `p:`, `ps:`, `pq:`, `pr:` and their kin:
    /// `http://www.wikidata.org/prop/direct/P31`, `…/prop/P31`,
    /// `…/prop/statement/value/P31`, `…/prop/qualifier/P580`, … (any host,
    /// http or https). The entity URI of a property is [`Self::from_uri`].
    pub fn from_predicate_uri(uri: &str) -> Result<Self, EntityIdError> {
        let path = strip_prefix_ignore_case(uri, "https://")
            .or_else(|| strip_prefix_ignore_case(uri, "http://"))
            .and_then(|rest| rest.split_once('/'))
            .map(|(_host, path)| path);
        path.and_then(|path| path.strip_prefix("prop/"))
            .and_then(|path| path.rsplit('/').next())
            .ok_or_else(|| invalid(uri))?
            .parse()
            .map_err(|_| invalid(uri))
    }
}

impl MediaInfoId {
    /// The MediaInfo entity of the file page with the given page ID.
    pub const fn from_page_id(page_id: u64) -> Self {
        Self(page_id)
    }

    /// The page ID of the file page.
    pub const fn page_id(self) -> u64 {
        self.0
    }
}

macro_rules! lexeme_sub_id {
    ($(#[$doc:meta])* $name:ident, $kind:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name {
            lexeme: LexemeId,
            number: u64,
        }

        impl $name {
            /// The kind of entity this type identifies.
            pub const KIND: EntityKind = EntityKind::$kind;

            pub const fn new(lexeme: LexemeId, number: u64) -> Self {
                Self { lexeme, number }
            }

            /// The lexeme this belongs to.
            pub const fn lexeme(self) -> LexemeId {
                self.lexeme
            }

            /// The number after the dash (`2` for `L7-F2`).
            pub const fn number(self) -> u64 {
                self.number
            }

            /// Parses leniently; see [`EntityId::parse_lenient`].
            pub fn parse_lenient(s: &str) -> Result<Self, EntityIdError> {
                EntityId::parse_lenient(s)
                    .and_then(|id| Self::try_from(id).map_err(|_| wrong_kind(s, Self::KIND)))
            }

            /// Parses an entity URI.
            pub fn from_uri(uri: &str) -> Result<Self, EntityIdError> {
                EntityId::from_uri(uri)
                    .and_then(|id| Self::try_from(id).map_err(|_| wrong_kind(uri, Self::KIND)))
            }

            /// The concept URI.
            pub fn uri(self) -> String {
                EntityId::from(self).uri()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}-{}{}", self.lexeme, Self::KIND.prefix(), self.number)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, f)
            }
        }

        impl FromStr for $name {
            type Err = EntityIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<EntityId>()
                    .and_then(|id| Self::try_from(id).map_err(|_| wrong_kind(s, Self::KIND)))
            }
        }

        impl From<$name> for EntityId {
            fn from(id: $name) -> Self {
                EntityId::$kind(id)
            }
        }

        impl TryFrom<EntityId> for $name {
            type Error = EntityIdError;

            fn try_from(id: EntityId) -> Result<Self, Self::Error> {
                match id {
                    EntityId::$kind(id) => Ok(id),
                    other => Err(wrong_kind(&other.to_string(), Self::KIND)),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserializer.deserialize_any(IdVisitor::<Self>::new())
            }
        }

        impl FromIdInput for $name {
            fn from_str_input(s: &str) -> Result<Self, EntityIdError> {
                Self::parse_lenient(s)
            }

            fn from_number(_number: u64) -> Option<Self> {
                None
            }
        }
    };
}

lexeme_sub_id!(
    /// A lexeme form ID, `L7-F2`.
    FormId,
    Form
);
lexeme_sub_id!(
    /// A lexeme sense ID, `L7-S1`.
    SenseId,
    Sense
);

/// An ID of any kind of entity.
///
/// Sorts by kind first (in the order of [`EntityKind`]), then by number.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntityId {
    Item(ItemId),
    Property(PropertyId),
    Lexeme(LexemeId),
    Form(FormId),
    Sense(SenseId),
    MediaInfo(MediaInfoId),
    EntitySchema(EntitySchemaId),
}

impl EntityId {
    /// The kind of entity.
    pub const fn kind(self) -> EntityKind {
        match self {
            Self::Item(_) => EntityKind::Item,
            Self::Property(_) => EntityKind::Property,
            Self::Lexeme(_) => EntityKind::Lexeme,
            Self::Form(_) => EntityKind::Form,
            Self::Sense(_) => EntityKind::Sense,
            Self::MediaInfo(_) => EntityKind::MediaInfo,
            Self::EntitySchema(_) => EntityKind::EntitySchema,
        }
    }

    /// The number of the ID (`42` for `Q42`). For a form or sense, the number
    /// of its lexeme; see [`Self::page_entity`].
    pub const fn number(self) -> u64 {
        match self {
            Self::Item(id) => id.get(),
            Self::Property(id) => id.get(),
            Self::Lexeme(id) => id.get(),
            Self::Form(id) => id.lexeme().get(),
            Self::Sense(id) => id.lexeme().get(),
            Self::MediaInfo(id) => id.get(),
            Self::EntitySchema(id) => id.get(),
        }
    }

    /// The entity whose page this entity lives on: the lexeme for a form or
    /// sense, the entity itself otherwise.
    pub const fn page_entity(self) -> Self {
        match self {
            Self::Form(id) => Self::Lexeme(id.lexeme()),
            Self::Sense(id) => Self::Lexeme(id.lexeme()),
            other => other,
        }
    }

    /// The MediaWiki namespace of the entity's page; see
    /// [`EntityKind::namespace`].
    pub const fn namespace(self) -> i64 {
        self.kind().namespace()
    }

    /// The item ID, if this is one.
    pub const fn as_item(self) -> Option<ItemId> {
        match self {
            Self::Item(id) => Some(id),
            _ => None,
        }
    }

    /// The property ID, if this is one.
    pub const fn as_property(self) -> Option<PropertyId> {
        match self {
            Self::Property(id) => Some(id),
            _ => None,
        }
    }

    /// Parses leniently; see the [module documentation](self). Unlike the
    /// kind-specific types, a bare number is rejected: it is ambiguous.
    pub fn parse_lenient(s: &str) -> Result<Self, EntityIdError> {
        let trimmed = s.trim();
        let candidate = Self::id_part_of_uri(trimmed)
            .or_else(|| strip_prefix_ignore_case(trimmed, "wd:"))
            .unwrap_or(trimmed);
        parse_id(candidate, true).ok_or_else(|| invalid(s))
    }

    /// Parses an entity URI as found in SPARQL results or RDF:
    /// `http(s)://<host>/entity/<ID>`. The page URL forms
    /// (`/wiki/Q42`, `/wiki/Special:EntityPage/Q42`) are accepted as well.
    pub fn from_uri(uri: &str) -> Result<Self, EntityIdError> {
        Self::id_part_of_uri(uri)
            .and_then(|id| parse_id(id, false))
            .ok_or_else(|| invalid(uri))
    }

    /// The ID part of an entity URI or page URL, without validating it.
    fn id_part_of_uri(uri: &str) -> Option<&str> {
        let rest = strip_prefix_ignore_case(uri, "https://")
            .or_else(|| strip_prefix_ignore_case(uri, "http://"))?;
        let (_host, path) = rest.split_once('/')?;
        let id = path
            .strip_prefix("entity/")
            .or_else(|| path.strip_prefix("wiki/Special:EntityPage/"))
            .or_else(|| path.strip_prefix("wiki/"))?;
        let id = id.split(['?', '#']).next().unwrap_or(id);
        // A page title carries its namespace: `Property:P31`, `Lexeme:L7`.
        Some(id.rsplit_once(':').map_or(id, |(_namespace, id)| id))
    }

    /// The concept URI: `http://www.wikidata.org/entity/Q42`, or
    /// `https://commons.wikimedia.org/entity/M123` for MediaInfo.
    pub fn uri(self) -> String {
        let base = match self {
            Self::MediaInfo(_) => COMMONS_ENTITY_PREFIX,
            _ => WIKIDATA_ENTITY_PREFIX,
        };
        format!("{base}{self}")
    }

    /// The title of the entity's page (`Property:P31`, `Lexeme:L7`), with the
    /// fragment for a form or sense (`Lexeme:L7#F2`), as used in a wiki link.
    /// `None` for MediaInfo, whose page is the file page.
    pub fn page_title(self) -> Option<String> {
        let page = self.page_entity();
        let prefix = match page.kind() {
            EntityKind::MediaInfo => return None,
            kind => kind.page_prefix().unwrap_or(""),
        };
        let fragment = match self {
            Self::Form(id) => format!("#F{}", id.number()),
            Self::Sense(id) => format!("#S{}", id.number()),
            _ => String::new(),
        };
        Some(format!("{prefix}{page}{fragment}"))
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Item(id) => id.fmt(f),
            Self::Property(id) => id.fmt(f),
            Self::Lexeme(id) => id.fmt(f),
            Self::Form(id) => id.fmt(f),
            Self::Sense(id) => id.fmt(f),
            Self::MediaInfo(id) => id.fmt(f),
            Self::EntitySchema(id) => id.fmt(f),
        }
    }
}

impl fmt::Debug for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for EntityId {
    type Err = EntityIdError;

    /// Parses the canonical form (`Q42`, `L7-F2`) only.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_id(s, false).ok_or_else(|| invalid(s))
    }
}

impl PartialEq<str> for EntityId {
    fn eq(&self, other: &str) -> bool {
        other.parse::<Self>().is_ok_and(|id| id == *self)
    }
}

impl PartialEq<&str> for EntityId {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl Serialize for EntityId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EntityId {
    /// Accepts a string, leniently (as [`Self::parse_lenient`]).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(IdVisitor::<Self>::new())
    }
}

impl FromIdInput for EntityId {
    fn from_str_input(s: &str) -> Result<Self, EntityIdError> {
        Self::parse_lenient(s)
    }

    fn from_number(_number: u64) -> Option<Self> {
        None
    }
}

#[cfg(feature = "wikibase")]
impl From<EntityKind> for wikibase::EntityType {
    fn from(kind: EntityKind) -> Self {
        match kind {
            EntityKind::Item => Self::Item,
            EntityKind::Property => Self::Property,
            EntityKind::Lexeme => Self::Lexeme,
            EntityKind::Form => Self::LexemeForm,
            EntityKind::Sense => Self::LexemeSense,
            EntityKind::MediaInfo => Self::MediaInfo,
            EntityKind::EntitySchema => Self::EntitySchema,
        }
    }
}

/// Serializes the kind-specific IDs as bare numbers (`42`) instead of strings
/// (`"Q42"`), for JSON consumers or storage that use the number.
///
/// ```ignore
/// #[derive(Serialize, Deserialize)]
/// struct Entry {
///     #[serde(with = "wikimisc::entity_id::serde_numeric")]
///     q: ItemId,
///     #[serde(with = "wikimisc::entity_id::serde_numeric::option")]
///     p: Option<PropertyId>,
/// }
/// ```
///
/// Deserializing accepts a number or a string, as the default does.
pub mod serde_numeric {
    use super::{FromIdInput, IdVisitor, NumericId};
    use serde::{Deserializer, Serializer};

    pub fn serialize<T: NumericId, S: Serializer>(
        id: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(id.number())
    }

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: NumericId + FromIdInput,
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(IdVisitor::<T>::new())
    }

    /// The same for `Option`s; `None` is `null`.
    pub mod option {
        use super::super::{FromIdInput, NumericId};
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<T: NumericId, S: Serializer>(
            id: &Option<T>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match id {
                Some(id) => serializer.serialize_some(&id.number()),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
        where
            T: NumericId + FromIdInput,
            D: Deserializer<'de>,
        {
            #[derive(serde::Deserialize)]
            #[serde(untagged)]
            enum Raw {
                Number(u64),
                Text(String),
            }
            match Option::<Raw>::deserialize(deserializer)? {
                None => Ok(None),
                Some(Raw::Number(number)) => T::from_number(number)
                    .map(Some)
                    .ok_or_else(|| serde::de::Error::custom(format!("invalid ID number {number}"))),
                Some(Raw::Text(text)) => T::from_str_input(&text)
                    .map(Some)
                    .map_err(serde::de::Error::custom),
            }
        }
    }
}

/// The kind-specific ID types that are a prefix and a number: [`ItemId`],
/// [`PropertyId`], [`LexemeId`], [`MediaInfoId`], [`EntitySchemaId`]. Sealed.
pub trait NumericId: Copy + sealed::Sealed {
    /// The number of the ID.
    fn number(self) -> u64;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! numeric_id_trait {
    ($($name:ident),*) => {$(
        impl sealed::Sealed for $name {}
        impl NumericId for $name {
            fn number(self) -> u64 {
                self.0
            }
        }
    )*};
}
numeric_id_trait!(ItemId, PropertyId, LexemeId, MediaInfoId, EntitySchemaId);

/// How the serde visitor builds an ID from a string or a number.
#[doc(hidden)]
pub trait FromIdInput: Sized {
    fn from_str_input(s: &str) -> Result<Self, EntityIdError>;
    fn from_number(number: u64) -> Option<Self>;
}

struct IdVisitor<T>(std::marker::PhantomData<T>);

impl<T> IdVisitor<T> {
    const fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: FromIdInput> Visitor<'_> for IdVisitor<T> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Wikibase entity ID")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<T, E> {
        T::from_str_input(v).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<T, E> {
        T::from_number(v).ok_or_else(|| E::custom(format!("invalid entity ID number {v}")))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<T, E> {
        u64::try_from(v)
            .ok()
            .and_then(T::from_number)
            .ok_or_else(|| E::custom(format!("invalid entity ID number {v}")))
    }
}

fn invalid(input: &str) -> EntityIdError {
    EntityIdError::Invalid(input.to_string())
}

fn wrong_kind(input: &str, expected: EntityKind) -> EntityIdError {
    EntityIdError::WrongKind {
        input: input.to_string(),
        expected,
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| s.get(prefix.len()..))
        .flatten()
}

/// A positive number of ASCII digits. Leading zeros are allowed only when
/// `lenient`; a sign never is.
fn parse_number(digits: &str, lenient: bool) -> Option<u64> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !lenient && digits.starts_with('0') {
        return None;
    }
    digits.parse().ok().filter(|&number| number > 0)
}

/// A full ID: `<prefix><number>` or `L<number>-<F|S><number>`. `lenient`
/// allows a lowercase prefix and leading zeros.
fn parse_id(s: &str, lenient: bool) -> Option<EntityId> {
    if let Some((lexeme, sub)) = s.split_once('-') {
        let lexeme = LexemeId(parse_prefixed(lexeme, 'L', lenient)?);
        let mut chars = sub.chars();
        let marker = chars.next()?;
        let marker = if lenient {
            marker.to_ascii_uppercase()
        } else {
            marker
        };
        let number = parse_number(chars.as_str(), lenient)?;
        return match marker {
            'F' => Some(EntityId::Form(FormId::new(lexeme, number))),
            'S' => Some(EntityId::Sense(SenseId::new(lexeme, number))),
            _ => None,
        };
    }
    let mut chars = s.chars();
    let prefix = chars.next()?;
    let prefix = if lenient {
        prefix.to_ascii_uppercase()
    } else {
        prefix
    };
    let number = parse_number(chars.as_str(), lenient)?;
    Some(match EntityKind::from_prefix(prefix)? {
        EntityKind::Item => EntityId::Item(ItemId(number)),
        EntityKind::Property => EntityId::Property(PropertyId(number)),
        EntityKind::Lexeme => EntityId::Lexeme(LexemeId(number)),
        EntityKind::MediaInfo => EntityId::MediaInfo(MediaInfoId(number)),
        EntityKind::EntitySchema => EntityId::EntitySchema(EntitySchemaId(number)),
        EntityKind::Form | EntityKind::Sense => return None,
    })
}

fn parse_prefixed(s: &str, prefix: char, lenient: bool) -> Option<u64> {
    let mut chars = s.chars();
    let first = chars.next()?;
    let matches = if lenient {
        first.eq_ignore_ascii_case(&prefix)
    } else {
        first == prefix
    };
    matches
        .then(|| parse_number(chars.as_str(), lenient))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_parsing_accepts_only_the_canonical_form() {
        assert_eq!("Q42".parse::<ItemId>().unwrap(), ItemId::new(42));
        assert_eq!("P31".parse::<PropertyId>().unwrap(), PropertyId::new(31));
        for bad in [
            "q42",
            "42",
            " Q42",
            "Q42 ",
            "Q042",
            "Q0",
            "Q",
            "Q+5",
            "Q-5",
            "Q4}",
            "Q٤٢",
            "",
            "Q99999999999999999999",
        ] {
            assert!(bad.parse::<ItemId>().is_err(), "{bad:?} must be rejected");
            assert!(bad.parse::<EntityId>().is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn wrong_kind_is_reported_as_such() {
        assert_eq!(
            "P31".parse::<ItemId>(),
            Err(EntityIdError::WrongKind {
                input: "P31".to_string(),
                expected: EntityKind::Item
            })
        );
        assert!(matches!(
            "X31".parse::<ItemId>(),
            Err(EntityIdError::Invalid(_))
        ));
    }

    #[test]
    fn every_kind_round_trips() {
        for s in ["Q42", "P31", "L7", "L7-F2", "L7-S1", "M123", "E10"] {
            let id: EntityId = s.parse().unwrap();
            assert_eq!(id.to_string(), s);
            assert_eq!(format!("{id:?}"), s);
        }
    }

    #[test]
    fn lenient_parsing() {
        for s in [
            "Q42",
            " q42 ",
            "Q0042",
            "42",
            "http://www.wikidata.org/entity/Q42",
            "https://www.wikidata.org/entity/Q42",
            "https://www.wikidata.org/wiki/Q42",
            "https://www.wikidata.org/wiki/Special:EntityPage/Q42",
            "https://test.wikidata.org/entity/Q42",
            "wd:Q42",
        ] {
            assert_eq!(ItemId::parse_lenient(s).unwrap(), ItemId::new(42), "{s:?}");
        }
        assert_eq!(
            PropertyId::parse_lenient("https://www.wikidata.org/wiki/Property:P31").unwrap(),
            PropertyId::new(31)
        );
        assert_eq!(
            EntityId::parse_lenient("l7-f2").unwrap().to_string(),
            "L7-F2"
        );
        for bad in ["Q0", "0", "Q+5", "Q-5", "QQ5", "", "Q4}", "P31", "Q٤٢"] {
            assert!(ItemId::parse_lenient(bad).is_err(), "{bad:?}");
        }
        // A bare number is ambiguous without a kind.
        assert!(EntityId::parse_lenient("42").is_err());
    }

    #[test]
    fn uris() {
        assert_eq!(
            EntityId::from_uri("http://www.wikidata.org/entity/P31").unwrap(),
            EntityId::Property(PropertyId::new(31))
        );
        assert_eq!(
            ItemId::from_uri("https://www.wikidata.org/entity/Q42").unwrap(),
            ItemId::new(42)
        );
        assert!(ItemId::from_uri("Q42").is_err());
        assert!(ItemId::from_uri("http://www.wikidata.org/entity/P31").is_err());
        assert!(ItemId::from_uri("http://www.wikidata.org/entity/statement/Q1-abc").is_err());
        assert!(EntityId::from_uri("http://www.wikidata.org/entity/Q42x").is_err());
        assert_eq!(ItemId::new(42).uri(), "http://www.wikidata.org/entity/Q42");
        assert_eq!(
            MediaInfoId::new(5).uri(),
            "https://commons.wikimedia.org/entity/M5"
        );
    }

    #[test]
    fn predicate_uris() {
        for uri in [
            "http://www.wikidata.org/prop/direct/P31",
            "http://www.wikidata.org/prop/P31",
            "https://www.wikidata.org/prop/statement/value-normalized/P31",
            "http://www.wikidata.org/prop/qualifier/P31",
            "http://www.wikidata.org/prop/direct-normalized/P31",
        ] {
            assert_eq!(
                PropertyId::from_predicate_uri(uri).unwrap(),
                PropertyId::new(31),
                "{uri}"
            );
        }
        for bad in [
            "http://www.wikidata.org/entity/P31",
            "http://www.wikidata.org/prop/direct/Q31",
            "http://www.wikidata.org/prop/direct/",
            "http://www.wikidata.org/prop/direct/P031",
            "prop/direct/P31",
            "P31",
        ] {
            assert!(PropertyId::from_predicate_uri(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn page_titles() {
        let title = |s: &str| s.parse::<EntityId>().unwrap().page_title();
        assert_eq!(title("Q42").as_deref(), Some("Q42"));
        assert_eq!(title("P31").as_deref(), Some("Property:P31"));
        assert_eq!(title("L7").as_deref(), Some("Lexeme:L7"));
        assert_eq!(title("L7-F2").as_deref(), Some("Lexeme:L7#F2"));
        assert_eq!(title("E10").as_deref(), Some("EntitySchema:E10"));
        assert_eq!(title("M5"), None);
    }

    #[test]
    fn kinds() {
        let id: EntityId = "L7-S1".parse().unwrap();
        assert_eq!(id.kind(), EntityKind::Sense);
        assert_eq!(id.namespace(), 146);
        assert_eq!(id.number(), 7);
        assert_eq!(id.page_entity().to_string(), "L7");
        assert_eq!(EntityKind::Property.namespace(), 120);
        assert_eq!(
            EntityKind::from_entity_type("mediainfo"),
            Some(EntityKind::MediaInfo)
        );
        assert_eq!(EntityKind::from_prefix('q'), None);
    }

    #[test]
    fn ordering_is_by_kind_then_number() {
        let mut ids: Vec<EntityId> = ["P2", "Q10", "Q9", "P1"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        ids.sort();
        let sorted: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(sorted, ["Q9", "Q10", "P1", "P2"]);
    }

    #[test]
    fn conversions() {
        let id = EntityId::from(ItemId::new(42));
        assert_eq!(ItemId::try_from(id).unwrap(), ItemId::new(42));
        assert!(PropertyId::try_from(id).is_err());
        assert_eq!(id.as_item(), Some(ItemId::new(42)));
        assert_eq!(id.as_property(), None);
        assert!(ItemId::new(42) == "Q42");
        assert!(ItemId::new(42) != "q42");
    }

    #[test]
    fn serde_uses_strings_and_reads_leniently() {
        assert_eq!(serde_json::to_value(ItemId::new(42)).unwrap(), json!("Q42"));
        assert_eq!(
            serde_json::from_value::<ItemId>(json!("q42")).unwrap(),
            ItemId::new(42)
        );
        assert_eq!(
            serde_json::from_value::<ItemId>(json!(42)).unwrap(),
            ItemId::new(42)
        );
        assert!(serde_json::from_value::<ItemId>(json!(0)).is_err());
        assert!(serde_json::from_value::<ItemId>(json!("P31")).is_err());
        assert_eq!(
            serde_json::from_value::<EntityId>(json!("L1-F2")).unwrap(),
            "L1-F2".parse::<EntityId>().unwrap()
        );
    }

    #[test]
    fn serde_numeric() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Row {
            #[serde(with = "super::serde_numeric")]
            q: ItemId,
            #[serde(with = "super::serde_numeric::option", default)]
            p: Option<PropertyId>,
        }
        let row = Row {
            q: ItemId::new(42),
            p: None,
        };
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(value, json!({"q": 42, "p": null}));
        assert_eq!(serde_json::from_value::<Row>(value).unwrap(), row);
        assert_eq!(
            serde_json::from_value::<Row>(json!({"q": "Q42", "p": "P31"})).unwrap(),
            Row {
                q: ItemId::new(42),
                p: Some(PropertyId::new(31))
            }
        );
    }
}
