//! Stable, UUID-backed entity identity.
//!
//! Blender object *names* are mutable and non-unique across data-block kinds,
//! so they are never the canonical identifier. Every entity the bridge touches
//! carries an `mcp_id` custom property holding one of these UUIDs.

use std::{fmt, marker::PhantomData, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use uuid::Uuid;

/// Marker trait for the entity kind an [`Id`] points at.
pub trait EntityKind: Copy + Clone + fmt::Debug + Eq + Ord + std::hash::Hash + 'static {
    /// Stable, human-readable kind name, used in errors and diagnostics.
    const NAME: &'static str;
}

macro_rules! entity_kinds {
    ($($kind:ident => $name:literal, $alias:ident;)*) => {
        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $kind;
            impl EntityKind for $kind {
                const NAME: &'static str = $name;
            }
            #[doc = concat!("Stable identifier for a Blender ", $name, ".")]
            pub type $alias = Id<$kind>;
        )*

        /// Runtime-tagged entity kind, used by cache and diff machinery.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
        pub enum AnyKind {
            $(
                #[serde(rename = $name)]
                $kind,
            )*
        }

        impl AnyKind {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(AnyKind::$kind => $name,)*
                }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($name => Some(AnyKind::$kind),)*
                    _ => None,
                }
            }
        }

        impl fmt::Display for AnyKind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

entity_kinds! {
    ObjectKind      => "object",       ObjectId;
    MeshKind        => "mesh",         MeshId;
    MaterialKind    => "material",     MaterialId;
    CollectionKind  => "collection",   CollectionId;
    NodeTreeKind    => "node_tree",    NodeTreeId;
    NodeKind        => "node",         NodeId;
    ActionKind      => "action",       ActionId;
    ArmatureKind    => "armature",     ArmatureId;
    BoneKind        => "bone",         BoneId;
    CameraKind      => "camera",       CameraId;
    LightKind       => "light",        LightId;
    ImageKind       => "image",        ImageId;
    TextureKind     => "texture",      TextureId;
    ModifierKind    => "modifier",     ModifierId;
    ConstraintKind  => "constraint",   ConstraintId;
    SceneKind       => "scene",        SceneId;
    WorldKind       => "world",        WorldId;
    AssetKind       => "asset",        AssetId;
    ArtifactKind    => "artifact",     ArtifactId;
}

/// A phantom-typed UUID. `Id<ObjectKind>` and `Id<MaterialKind>` are distinct
/// types, so an object id can never be passed where a material id is expected.
pub struct Id<K: EntityKind> {
    uuid: Uuid,
    _kind: PhantomData<fn() -> K>,
}

impl<K: EntityKind> Id<K> {
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self {
            uuid,
            _kind: PhantomData,
        }
    }

    pub fn new() -> Self {
        Self::from_uuid(Uuid::new_v4())
    }

    pub const fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub const fn kind(&self) -> &'static str {
        K::NAME
    }

    /// Reinterpret this id as pointing at another entity kind.
    ///
    /// Blender routinely exposes one logical entity through several data-blocks
    /// (an object and the camera data it owns, for instance); this is the only
    /// sanctioned way to move between them.
    pub const fn retag<T: EntityKind>(&self) -> Id<T> {
        Id::from_uuid(self.uuid)
    }
}

impl<K: EntityKind> Default for Id<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: EntityKind> Clone for Id<K> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K: EntityKind> Copy for Id<K> {}
impl<K: EntityKind> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.uuid == other.uuid
    }
}
impl<K: EntityKind> Eq for Id<K> {}
impl<K: EntityKind> PartialOrd for Id<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<K: EntityKind> Ord for Id<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.uuid.cmp(&other.uuid)
    }
}
impl<K: EntityKind> std::hash::Hash for Id<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
    }
}

impl<K: EntityKind> fmt::Debug for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", K::NAME, self.uuid)
    }
}

impl<K: EntityKind> fmt::Display for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.uuid, f)
    }
}

impl<K: EntityKind> FromStr for Id<K> {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self::from_uuid)
    }
}

impl<K: EntityKind> Serialize for Id<K> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.uuid)
    }
}

impl<'de, K: EntityKind> Deserialize<'de> for Id<K> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Uuid::parse_str(&raw).map(Self::from_uuid).map_err(|_| {
            D::Error::custom(format!(
                "`{raw}` is not a valid {} id (expected a UUID)",
                K::NAME
            ))
        })
    }
}

impl<K: EntityKind> schemars::JsonSchema for Id<K> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("{}Id", K::NAME).into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": "uuid",
            "description": format!("Stable UUID of a Blender {}.", K::NAME),
        })
    }
}

/// How a caller points at an entity: by stable id, or by current Blender name.
///
/// Serialised as a bare string. Anything that parses as a UUID is treated as an
/// id; everything else is a name lookup. Names are a convenience for humans and
/// for first contact with a scene the server has not indexed yet -- ids are the
/// contract.
pub struct Ref<K: EntityKind> {
    inner: RefInner,
    _kind: PhantomData<fn() -> K>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RefInner {
    Id(Uuid),
    Name(String),
}

impl<K: EntityKind> Ref<K> {
    pub fn id(id: Id<K>) -> Self {
        Self {
            inner: RefInner::Id(id.uuid()),
            _kind: PhantomData,
        }
    }

    pub fn name(name: impl Into<String>) -> Self {
        Self {
            inner: RefInner::Name(name.into()),
            _kind: PhantomData,
        }
    }

    /// The id this reference resolves to, if it was given as one.
    pub fn as_id(&self) -> Option<Id<K>> {
        match &self.inner {
            RefInner::Id(u) => Some(Id::from_uuid(*u)),
            RefInner::Name(_) => None,
        }
    }

    pub fn as_name(&self) -> Option<&str> {
        match &self.inner {
            RefInner::Name(n) => Some(n),
            RefInner::Id(_) => None,
        }
    }

    pub const fn kind(&self) -> &'static str {
        K::NAME
    }

    /// Reinterpret this reference as pointing at another entity kind.
    pub fn retag<T: EntityKind>(&self) -> Ref<T> {
        Ref {
            inner: self.inner.clone(),
            _kind: PhantomData,
        }
    }
}

impl<K: EntityKind> Clone for Ref<K> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _kind: PhantomData,
        }
    }
}
impl<K: EntityKind> PartialEq for Ref<K> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}
impl<K: EntityKind> Eq for Ref<K> {}
impl<K: EntityKind> std::hash::Hash for Ref<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl<K: EntityKind> fmt::Debug for Ref<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", K::NAME, self)
    }
}

impl<K: EntityKind> fmt::Display for Ref<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            RefInner::Id(u) => fmt::Display::fmt(u, f),
            RefInner::Name(n) => f.write_str(n),
        }
    }
}

impl<K: EntityKind> From<Id<K>> for Ref<K> {
    fn from(id: Id<K>) -> Self {
        Self::id(id)
    }
}

impl<K: EntityKind> From<&str> for Ref<K> {
    fn from(s: &str) -> Self {
        match Uuid::parse_str(s) {
            Ok(u) => Self {
                inner: RefInner::Id(u),
                _kind: PhantomData,
            },
            Err(_) => Self::name(s),
        }
    }
}

impl<K: EntityKind> Serialize for Ref<K> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.inner {
            RefInner::Id(u) => s.collect_str(u),
            RefInner::Name(n) => s.serialize_str(n),
        }
    }
}

impl<'de, K: EntityKind> Deserialize<'de> for Ref<K> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        if raw.is_empty() {
            return Err(D::Error::custom(format!("empty {} reference", K::NAME)));
        }
        Ok(Self::from(raw.as_str()))
    }
}

impl<K: EntityKind> schemars::JsonSchema for Ref<K> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("{}Ref", K::NAME).into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "description": format!(
                "Reference to a Blender {kind}: either its stable `mcp_id` UUID (preferred) or its current name.",
                kind = K::NAME
            ),
        })
    }
}

pub type ObjectRef = Ref<ObjectKind>;
pub type MaterialRef = Ref<MaterialKind>;
pub type CollectionRef = Ref<CollectionKind>;
pub type NodeTreeRef = Ref<NodeTreeKind>;
pub type NodeRef = Ref<NodeKind>;
pub type ActionRef = Ref<ActionKind>;
pub type ArmatureRef = Ref<ArmatureKind>;
pub type CameraRef = Ref<CameraKind>;
pub type LightRef = Ref<LightKind>;
pub type ImageRef = Ref<ImageKind>;
pub type ModifierRef = Ref<ModifierKind>;
pub type SceneRef = Ref<SceneKind>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_round_trips_ids_and_names() {
        let id = ObjectId::new();
        let as_ref = ObjectRef::id(id);
        let json = serde_json::to_string(&as_ref).unwrap();
        let back: ObjectRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_id(), Some(id));

        let named: ObjectRef = serde_json::from_str("\"Cube\"").unwrap();
        assert_eq!(named.as_name(), Some("Cube"));
        assert_eq!(named.as_id(), None);
    }

    #[test]
    fn ids_reject_non_uuid() {
        assert!(serde_json::from_str::<ObjectId>("\"Cube\"").is_err());
    }

    #[test]
    fn retag_preserves_uuid() {
        let obj = ObjectId::new();
        let cam: CameraId = obj.retag();
        assert_eq!(obj.uuid(), cam.uuid());
    }
}
