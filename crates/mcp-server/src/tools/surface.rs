//! Surface and opening tools.
//!
//! A mesh with a hundred thousand triangles is useless to a caller that only
//! wants to know where its walls are. These tools group coplanar faces into
//! regions, classify them by world-space orientation, and hand back the frame
//! a placement actually needs: a point, a normal, an in-plane tangent and the
//! extent of the region.
//!
//! Openings -- doors and windows -- come from metadata somebody authored, never
//! from guessing at gaps in geometry. A hole in a mesh is not a doorway, and a
//! system that decides it is will be wrong in a way nobody can debug.

use blender_protocol::{
    BlenderError, Page, Result, Validate,
    command::{Category, OpKind},
    ids::ObjectRef,
    math::Vec3,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::registry::ToolSpec;

const SCENE: Category = Category::Scene;

/// The orientation classes a planar region is sorted into.
const SURFACE_CLASSES: [&str; 4] = ["WALL", "FLOOR", "CEILING", "OTHER"];
/// The opening kinds `scene.openings.mark` accepts.
const OPENING_KINDS: [&str; 4] = ["DOOR", "WINDOW", "SERVICE_DOOR", "UNKNOWN"];

/// `scene.surface.inspect`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectSurfaces {
    /// The mesh object to read.
    pub object: ObjectRef,
    /// How far from vertical a face may lean and still count as a wall, and
    /// how far from level to still be a floor or a ceiling. Default 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt_degrees: Option<f64>,
    /// How closely two neighbouring faces must agree in direction to belong to
    /// one region. Default 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_tolerance_degrees: Option<f64>,
    /// How far out of a shared plane a face may sit and still join its
    /// neighbours, in scene units. Default 0.02.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane_tolerance: Option<f64>,
    /// Drop regions smaller than this, in square scene units. Default 0.25.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_area: Option<f64>,
    /// Return only regions of one class: `WALL`, `FLOOR`, `CEILING` or `OTHER`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    #[serde(flatten)]
    pub page: Page,
}

impl Validate for InspectSurfaces {
    fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("tilt_degrees", self.tilt_degrees),
            ("normal_tolerance_degrees", self.normal_tolerance_degrees),
            ("plane_tolerance", self.plane_tolerance),
            ("min_area", self.min_area),
        ] {
            let Some(value) = value else { continue };
            if !value.is_finite() || value < 0.0 {
                return Err(BlenderError::invalid_argument(format!(
                    "`{field}` must be a finite, non-negative number."
                ))
                .with_detail("field", field));
            }
        }
        // At or past 90 degrees every face leans within tolerance of vertical,
        // so everything would come back a wall and the answer would be useless.
        if self.tilt_degrees.is_some_and(|tilt| tilt >= 90.0) {
            return Err(BlenderError::invalid_argument(
                "`tilt_degrees` at or past 90 would classify every face as a wall.",
            )
            .with_detail("field", "tilt_degrees"));
        }
        if let Some(class) = &self.classification
            && !SURFACE_CLASSES.contains(&class.as_str())
        {
            return Err(BlenderError::invalid_enum(
                "classification",
                class.clone(),
                SURFACE_CLASSES,
            ));
        }
        self.page.validate()
    }
}

/// `scene.surface.raycast`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RaycastSurface {
    /// The objects to test, named explicitly. A ray is never cast at the whole
    /// scene, so a stray helper object cannot quietly become the answer.
    pub objects: Vec<ObjectRef>,
    /// Where the ray starts, in world space.
    pub origin: Vec3,
    /// Which way it points. Normalised server-side; length is ignored.
    pub direction: Vec3,
    /// How far to look, in scene units. Default 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_distance: Option<f64>,
}

impl Validate for RaycastSurface {
    fn validate(&self) -> Result<()> {
        if self.objects.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`objects` must name at least one object to cast against.",
            )
            .with_detail("field", "objects"));
        }
        let direction = self.direction;
        let length =
            (direction.x * direction.x + direction.y * direction.y + direction.z * direction.z)
                .sqrt();
        if !length.is_finite() || length < 1e-9 {
            return Err(BlenderError::invalid_argument("`direction` has no length.")
                .with_detail("field", "direction"));
        }
        if let Some(distance) = self.max_distance
            && (!distance.is_finite() || distance <= 0.0)
        {
            return Err(BlenderError::invalid_argument(
                "`max_distance` must be a positive number.",
            )
            .with_detail("field", "max_distance"));
        }
        Ok(())
    }
}

/// `scene.openings.inspect`
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectOpenings {
    /// Only openings on this object: its children, plus anything that names it
    /// as its host. Omit to list every marked opening in the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<ObjectRef>,
    /// Check exactly these objects instead of searching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectRef>,
}

impl Validate for InspectOpenings {}

/// `scene.openings.mark`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkOpening {
    /// The object standing in the opening -- a door slab, a window pane, or a
    /// box somebody put there to mark it.
    pub object: ObjectRef,
    /// `DOOR`, `WINDOW`, `SERVICE_DOOR` or `UNKNOWN`. `UNKNOWN` is a real
    /// answer; claim a kind only when it is true.
    pub kind: String,
    /// The wall it belongs to. Without it the opening is still listed, but not
    /// when that wall is asked for its own openings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<ObjectRef>,
}

impl Validate for MarkOpening {
    fn validate(&self) -> Result<()> {
        if !OPENING_KINDS.contains(&self.kind.as_str()) {
            return Err(BlenderError::invalid_enum(
                "kind",
                self.kind.clone(),
                OPENING_KINDS,
            ));
        }
        Ok(())
    }
}

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<InspectSurfaces>(
            "scene.surface.inspect",
            SCENE,
            OpKind::Read,
            "Inspect an object's surfaces",
            "Group an object's faces into planar regions and classify each as a wall, a floor, a \
             ceiling or other, in world space with the object's own rotation already applied. \
             Each region comes back with a centre, a normal, an in-plane tangent and its extent, \
             so a caller can place something against it without reading a single triangle. A \
             thousand triangles of one wall come back as one wall. Paginated.",
        ),
        ToolSpec::forward::<RaycastSurface>(
            "scene.surface.raycast",
            SCENE,
            OpKind::Read,
            "Cast a ray at named objects",
            "Cast one ray at an explicit list of objects and report the nearest hit: the object, \
             the world-space point and normal, the face index, the distance, and how that surface \
             is classified. Useful for dropping something onto whatever is beneath it.",
        ),
        ToolSpec::forward::<InspectOpenings>(
            "scene.openings.inspect",
            SCENE,
            OpKind::Read,
            "List doors and windows",
            "List the openings authored on an object, or in the whole file. Each comes back with \
             its kind, host, world bounds, centre, size and through-axis normal. Nothing here \
             looks for holes in geometry: an opening is one because somebody marked it, and where \
             nobody has, the answer says so rather than guessing.",
        ),
        ToolSpec::forward::<MarkOpening>(
            "scene.openings.mark",
            SCENE,
            OpKind::Write,
            "Mark an object as an opening",
            "Record that an object is a door or a window, so later calls can measure from it -- \
             right of the service door, above the loading bay. Two fixed custom properties and \
             one enumerated kind; no property name here comes from the caller.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(name: &str) -> ObjectRef {
        ObjectRef::name(name)
    }

    fn surfaces_of(name: &str) -> InspectSurfaces {
        InspectSurfaces {
            object: object(name),
            tilt_degrees: None,
            normal_tolerance_degrees: None,
            plane_tolerance: None,
            min_area: None,
            classification: None,
            page: Page::default(),
        }
    }

    #[test]
    fn a_tilt_that_makes_everything_a_wall_is_refused() {
        let mut params = surfaces_of("Building");
        params.tilt_degrees = Some(90.0);
        assert!(params.validate().is_err());
        params.tilt_degrees = Some(30.0);
        assert!(params.validate().is_ok());
    }

    #[test]
    fn a_classification_outside_the_set_is_refused() {
        let mut params = surfaces_of("Building");
        params.classification = Some("ROOF".into());
        assert!(params.validate().is_err());
        params.classification = Some("WALL".into());
        assert!(params.validate().is_ok());
    }

    #[test]
    fn a_ray_needs_a_target_and_a_direction() {
        let mut params = RaycastSurface {
            objects: Vec::new(),
            origin: Vec3 {
                x: 0.0,
                y: 0.0,
                z: 10.0,
            },
            direction: Vec3 {
                x: 0.0,
                y: 0.0,
                z: -1.0,
            },
            max_distance: None,
        };
        assert!(params.validate().is_err(), "no objects to cast against");

        params.objects = vec![object("Ground")];
        assert!(params.validate().is_ok());

        params.direction = Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        assert!(
            params.validate().is_err(),
            "a zero direction points nowhere"
        );
    }

    #[test]
    fn an_opening_kind_comes_from_the_enumerated_set() {
        let mut params = MarkOpening {
            object: object("Door"),
            kind: "DOOR".into(),
            host: Some(object("Wall")),
        };
        assert!(params.validate().is_ok());
        params.kind = "HATCH".into();
        assert!(params.validate().is_err());
    }
}
