//! External asset libraries.
//!
//! Everything here goes out to the internet, so every tool is classified as an
//! external side effect except the ones that only read metadata. Downloads land
//! in the managed downloads root; nothing fetched is ever executed, unpacked to
//! an arbitrary place, or installed as an add-on.

use std::sync::Arc;

use asset_providers::AssetProviders;
use blender_domain::material::{MapKind, PbrSpec, TextureMap};
use blender_protocol::{
    BlenderError, ErrorCode, Result, Validate,
    asset::{
        AssetType, DownloadAsset, DownloadedAsset, DownloadedFile, ImportAsset, SearchAssets,
        check_asset_id, check_provider_id,
    },
    command::{Category, OpKind},
    io::{ImportOptions, ManagedRoot},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::NoParams;
use crate::{config::Root, registry::ToolSpec, state::AppState};

const ASSETS: Category = Category::Assets;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::custom::<NoParams, _, _>(
            "asset.providers",
            ASSETS,
            OpKind::Read,
            "List asset providers",
            "The external asset libraries this server can reach, whether each one has credentials \
             configured, what it offers, and what its licence terms are in general. Start here \
             before searching.",
            |state: Arc<AppState>, _params| async move { providers(&state) },
        ),
        ToolSpec::custom::<SearchAssets, _, _>(
            "asset.search",
            ASSETS,
            OpKind::Read,
            "Search asset libraries",
            "Search one provider or every configured provider. Returns titles, licences, authors \
             and the variants available for download. Licence data is reported exactly as the \
             provider states it -- decide for yourself whether an asset suits your project.",
            |state: Arc<AppState>, params: SearchAssets| async move {
                let assets = require_providers(&state)?;
                let results = assets.search(&params).await?;
                to_value(&results)
            },
        ),
        ToolSpec::custom::<GetAssetParams, _, _>(
            "asset.get",
            ASSETS,
            OpKind::Read,
            "Get one asset",
            "Full detail for a single asset, including every downloadable variant and its \
             licence.",
            |state: Arc<AppState>, params: GetAssetParams| async move {
                let assets = require_providers(&state)?;
                let asset = assets.get(&params.provider, &params.asset_id).await?;
                to_value(&asset)
            },
        ),
        ToolSpec::custom::<DownloadAsset, _, _>(
            "asset.download",
            ASSETS,
            OpKind::ExternalSideEffect,
            "Download an asset",
            "Fetch an asset into the managed downloads directory without touching the scene. \
             Files are cached, so asking twice costs one download. Returns each file's path, size \
             and SHA-256. Use `asset.import` to bring one into Blender.",
            |state: Arc<AppState>, params: DownloadAsset| async move {
                let assets = require_downloads(&state)?;
                let downloaded = assets.download(&params).await?;
                to_value(&downloaded)
            },
        ),
        ToolSpec::custom::<ImportAsset, _, _>(
            "asset.import",
            ASSETS,
            OpKind::ExternalSideEffect,
            "Download an asset and bring it into the scene",
            "Download an asset and then use it: an HDRI becomes the world environment, a texture \
             set becomes a PBR material with the maps wired up and data maps loaded as Non-Color, \
             and a model is imported through the normal typed importer. Nothing downloaded is \
             executed, and no add-on is installed.",
            |state: Arc<AppState>, params: ImportAsset| async move { import(&state, params).await },
        ),
    ]
}

/// `asset.get`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetAssetParams {
    /// Provider id, from `asset.providers`.
    pub provider: String,
    /// The provider's own identifier for the asset, from a search result.
    pub asset_id: String,
}

impl Validate for GetAssetParams {
    fn validate(&self) -> Result<()> {
        check_provider_id(&self.provider)?;
        check_asset_id(&self.asset_id)
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| BlenderError::internal(error.to_string()))
}

fn providers(state: &AppState) -> Result<Value> {
    let assets = state.assets.as_ref();
    Ok(json!({
        "providers": assets.map(|assets| assets.list()).unwrap_or_default(),
        "downloads_enabled": state.config.allow_asset_downloads,
        "max_download_bytes": state.config.max_download_bytes,
        "downloads_root": state.config.root_path(Root::Downloads).display().to_string(),
        "notice": "Licence terms come from the provider and are reported unchanged. This server \
                   never installs add-ons or executes anything it downloads.",
    }))
}

fn require_providers(state: &AppState) -> Result<&AssetProviders> {
    state.assets.as_deref().ok_or_else(|| {
        BlenderError::new(
            ErrorCode::CapabilityUnavailable,
            "No asset providers are configured on this server.",
        )
    })
}

/// Downloads can be turned off entirely, in which case searching still works.
fn require_downloads(state: &AppState) -> Result<&AssetProviders> {
    if !state.config.allow_asset_downloads {
        return Err(BlenderError::new(
            ErrorCode::PermissionDenied,
            "Asset downloads are disabled on this server. Set BLENDER_MCP_ALLOW_ASSET_DOWNLOADS=1 \
             to enable them.",
        )
        .with_detail("environment_variable", "BLENDER_MCP_ALLOW_ASSET_DOWNLOADS"));
    }
    require_providers(state)
}

async fn import(state: &Arc<AppState>, params: ImportAsset) -> Result<Value> {
    let assets = require_downloads(state)?;
    let downloaded = assets.download(&params.download).await?;

    let used = match downloaded.asset.asset_type {
        AssetType::Hdri if params.apply_as_world => apply_hdri(state, &params, &downloaded).await?,
        AssetType::Hdri => load_images(state, &downloaded, false).await?,
        AssetType::Texture | AssetType::Material if params.build_material => {
            build_material(state, &params, &downloaded).await?
        }
        AssetType::Texture | AssetType::Material => load_images(state, &downloaded, true).await?,
        AssetType::Model => import_model(state, &params, &downloaded).await?,
    };

    Ok(json!({
        "asset": downloaded.asset,
        "files": downloaded.files,
        "from_cache": downloaded.from_cache,
        "total_bytes": downloaded.total_bytes,
        "applied": used,
        "license": downloaded.asset.license,
    }))
}

/// A downloaded file's path, as `image.load` and `io.import` expect it: relative
/// to the downloads root.
fn managed_path(file: &DownloadedFile) -> String {
    file.path.clone()
}

/// Load one image into Blender and return the name it ended up with.
async fn load_image(
    state: &Arc<AppState>,
    file: &DownloadedFile,
    colorspace: &str,
    name: Option<String>,
) -> Result<String> {
    let absolute = crate::config::resolve_managed_path(
        &state.config.root_path(Root::Downloads),
        &managed_path(file),
    )?;
    let mut args = json!({
        "source_path": absolute.display().to_string(),
        "colorspace": colorspace,
    });
    if let Some(name) = name {
        args["name"] = json!(name);
    }
    let result = state.client.call("image.load", args).await?;
    result
        .get("image")
        .and_then(|image| image.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| BlenderError::internal("`image.load` returned no image name"))
}

/// Load every image in a download without wiring anything up.
async fn load_images(
    state: &Arc<AppState>,
    downloaded: &DownloadedAsset,
    data_maps: bool,
) -> Result<Value> {
    let mut loaded = Vec::new();
    for file in &downloaded.files {
        if !is_image(file) {
            continue;
        }
        let colorspace = if data_maps {
            map_kind_for(file.map.as_deref()).map_or("sRGB", |kind| kind.colorspace())
        } else {
            "sRGB"
        };
        loaded.push(load_image(state, file, colorspace, None).await?);
    }
    Ok(json!({"action": "loaded_images", "images": loaded}))
}

async fn apply_hdri(
    state: &Arc<AppState>,
    params: &ImportAsset,
    downloaded: &DownloadedAsset,
) -> Result<Value> {
    let file = downloaded
        .files
        .iter()
        .find(|file| is_image(file))
        .ok_or_else(|| {
            BlenderError::new(
                ErrorCode::AssetDownloadFailed,
                "The download contains no image to use as an environment.",
            )
        })?;

    // An HDRI is radiance data with its own encoding; forcing a colour space
    // here would be wrong, so Blender's own choice for the format is kept.
    let image = load_image(state, file, "Linear Rec.709", params.name.clone()).await?;
    state
        .call_typed(
            "scene.world.update",
            &blender_protocol::scene::WorldSettings {
                hdri: Some(image.clone()),
                ..Default::default()
            },
        )
        .await?;

    Ok(json!({"action": "world_environment", "image": image}))
}

async fn build_material(
    state: &Arc<AppState>,
    params: &ImportAsset,
    downloaded: &DownloadedAsset,
) -> Result<Value> {
    let mut maps = Vec::new();
    for file in &downloaded.files {
        let Some(kind) = map_kind_for(file.map.as_deref()) else {
            continue;
        };
        let image = load_image(state, file, kind.colorspace(), None).await?;
        maps.push(TextureMap { kind, image });
    }

    if maps.is_empty() {
        return Err(BlenderError::new(
            ErrorCode::AssetProviderError,
            "None of the downloaded files could be identified as a texture map, so no material \
             was built. The files are on disk; wire them up with `shader.graph.build`.",
        )
        .with_detail_json(
            "files",
            &downloaded
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
        ));
    }

    let spec = PbrSpec {
        maps,
        base_color: None,
        roughness: None,
        metallic: None,
        uv_scale: None,
        normal_strength: None,
        displacement_scale: None,
    };
    let plan = spec.plan()?;

    let name = params
        .name
        .clone()
        .unwrap_or_else(|| downloaded.asset.title.clone());
    let created = state
        .call_raw(
            "material.create",
            json_map(json!({"name": name, "use_nodes": true})),
        )
        .await?;
    let material = created
        .get("material")
        .and_then(|material| material.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| BlenderError::internal("`material.create` returned no id"))?
        .to_string();

    state
        .call_raw(
            "shader.graph.build",
            json_map(json!({
                "material": material,
                "clear": true,
                "nodes": plan.nodes,
                "links": plan.links,
            })),
        )
        .await?;

    if let Some(collection) = &params.collection {
        tracing::debug!(
            ?collection,
            "a collection was given for a material import; ignored"
        );
    }

    Ok(json!({
        "action": "built_material",
        "material": material,
        "maps": spec_map_names(&spec),
    }))
}

async fn import_model(
    state: &Arc<AppState>,
    params: &ImportAsset,
    downloaded: &DownloadedAsset,
) -> Result<Value> {
    // The importable file is the model itself, not the textures beside it.
    let file = downloaded
        .files
        .iter()
        .find(|file| is_model(file))
        .ok_or_else(|| {
            BlenderError::new(
                ErrorCode::UnsupportedFormat,
                "The download contains no file Blender can import directly. Archives are left on \
                 disk rather than unpacked; extract it yourself and use `io.import`.",
            )
            .with_detail_json(
                "files",
                &downloaded
                    .files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<Vec<_>>(),
            )
        })?;

    let import = blender_protocol::io::Import {
        source: blender_protocol::io::ManagedPath::new(ManagedRoot::Downloads, managed_path(file)),
        format: None,
        options: ImportOptions {
            collection: params.collection.clone(),
            name_prefix: params.name.clone(),
            ..Default::default()
        },
    };
    import.validate()?;

    let absolute = crate::config::resolve_managed_path(
        &state.config.root_path(Root::Downloads),
        &import.source.path,
    )?;
    let mut args =
        serde_json::to_value(&import).map_err(|error| BlenderError::internal(error.to_string()))?;
    args["source_path"] = json!(absolute.display().to_string());
    let result = state.client.call("io.import", args).await?;

    Ok(json!({"action": "imported_model", "import": result}))
}

fn json_map(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn spec_map_names(spec: &PbrSpec) -> Vec<String> {
    spec.maps
        .iter()
        .map(|map| format!("{:?}", map.kind))
        .collect()
}

fn extension(file: &DownloadedFile) -> String {
    file.path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_image(file: &DownloadedFile) -> bool {
    matches!(
        extension(file).as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "tga" | "exr" | "hdr"
    )
}

fn is_model(file: &DownloadedFile) -> bool {
    matches!(
        extension(file).as_str(),
        "blend" | "gltf" | "glb" | "fbx" | "obj" | "ply" | "stl" | "usd" | "usda" | "usdc" | "usdz"
    )
}

/// Map a provider's map name onto the kind the material planner understands.
///
/// Poly Haven's names are the de-facto standard, and the aliases cover what
/// other libraries call the same thing. An unrecognised map is left out rather
/// than guessed at: wiring a mystery image into Base Color would be worse than
/// leaving it on disk.
fn map_kind_for(name: Option<&str>) -> Option<MapKind> {
    let name = name?.to_ascii_lowercase();
    let name = name.trim();
    Some(match name {
        "diffuse" | "diff" | "albedo" | "col" | "color" | "basecolor" | "base_color" => {
            MapKind::BaseColor
        }
        "rough" | "roughness" => MapKind::Roughness,
        "metal" | "metallic" | "metalness" => MapKind::Metallic,
        "nor_gl" | "nor" | "normal" | "normalgl" | "normal_gl" => MapKind::Normal,
        "disp" | "displacement" | "height" | "bump" => MapKind::Height,
        "ao" | "ambientocclusion" | "ambient_occlusion" => MapKind::AmbientOcclusion,
        "emission" | "emissive" => MapKind::Emission,
        "alpha" | "opacity" => MapKind::Alpha,
        "spec" | "specular" => MapKind::Specular,
        // DirectX normals are inverted on the green channel; wiring one in as a
        // normal map would produce lighting that is subtly, confusingly wrong.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, map: Option<&str>) -> DownloadedFile {
        DownloadedFile {
            path: path.to_string(),
            size_bytes: 1,
            sha256: "0".repeat(64),
            map: map.map(str::to_string),
            mime_type: None,
        }
    }

    #[test]
    fn there_is_no_tool_that_executes_anything() {
        for tool in tools() {
            let name = tool.name;
            assert!(
                !name.contains("exec")
                    && !name.contains("eval")
                    && !name.contains("script")
                    && !name.contains("install"),
                "{name}"
            );
        }
    }

    #[test]
    fn downloads_are_external_side_effects_and_searches_are_not() {
        let by_name = |name: &str| tools().into_iter().find(|t| t.name == name).unwrap().kind;
        assert_eq!(by_name("asset.search"), OpKind::Read);
        assert_eq!(by_name("asset.get"), OpKind::Read);
        assert_eq!(by_name("asset.download"), OpKind::ExternalSideEffect);
        assert_eq!(by_name("asset.import"), OpKind::ExternalSideEffect);
    }

    #[test]
    fn provider_map_names_reach_the_right_socket() {
        assert_eq!(map_kind_for(Some("Diffuse")), Some(MapKind::BaseColor));
        assert_eq!(map_kind_for(Some("nor_gl")), Some(MapKind::Normal));
        assert_eq!(map_kind_for(Some("Rough")), Some(MapKind::Roughness));
        assert_eq!(map_kind_for(Some("Displacement")), Some(MapKind::Height));
        assert_eq!(map_kind_for(Some("AO")), Some(MapKind::AmbientOcclusion));
    }

    #[test]
    fn an_unknown_map_is_left_alone_rather_than_guessed() {
        assert_eq!(
            map_kind_for(Some("nor_dx")),
            None,
            "DirectX normals are not GL normals"
        );
        assert_eq!(map_kind_for(Some("curiosity")), None);
        assert_eq!(map_kind_for(None), None);
    }

    #[test]
    fn data_maps_are_never_loaded_as_srgb() {
        for name in ["Rough", "nor_gl", "Displacement", "Metal", "AO"] {
            let kind = map_kind_for(Some(name)).unwrap();
            assert_eq!(kind.colorspace(), "Non-Color", "{name}");
        }
        assert_eq!(map_kind_for(Some("Diffuse")).unwrap().colorspace(), "sRGB");
    }

    #[test]
    fn file_kinds_are_told_apart_by_extension() {
        assert!(is_image(&file("a/b_diff_2k.jpg", Some("Diffuse"))));
        assert!(is_image(&file("a/env_4k.hdr", None)));
        assert!(!is_image(&file("a/model.glb", None)));
        assert!(is_model(&file("a/model.glb", None)));
        assert!(
            !is_model(&file("a/archive.zip", None)),
            "an archive is not something Blender can import directly"
        );
    }

    #[test]
    fn asset_identifiers_are_validated_before_any_request() {
        let bad = GetAssetParams {
            provider: "poly haven".into(),
            asset_id: "rock".into(),
        };
        assert!(bad.validate().is_err());
        let traversal = GetAssetParams {
            provider: "polyhaven".into(),
            asset_id: "../../etc/passwd".into(),
        };
        assert!(traversal.validate().is_err());
        assert!(
            GetAssetParams {
                provider: "polyhaven".into(),
                asset_id: "rocky_terrain_02".into(),
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn no_asset_tool_takes_a_free_form_payload() {
        for tool in tools() {
            let schema = serde_json::to_value(&*tool.schema).unwrap();
            assert_eq!(schema["type"], "object", "{}", tool.name);
        }
    }
}
