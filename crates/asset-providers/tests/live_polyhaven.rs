//! Live tests against the real Poly Haven API.
//!
//! Everything else in this crate is tested against canned responses, which
//! proves the parsing is self-consistent but not that it matches what the
//! provider actually sends. These tests close that gap.
//!
//! They are `#[ignore]`d, so an ordinary `cargo test` never touches the
//! network:
//!
//!     cargo test -p asset-providers --test live_polyhaven -- --ignored --nocapture
//!
//! Only one small file is ever downloaded (a 1k HDR, a couple of megabytes).
//! Everything else stops at the plan, which is where the API-shape assumptions
//! live anyway.

use std::{path::PathBuf, sync::Arc};

use asset_providers::{
    AssetProvider, DownloadPolicy, Downloader, Fetcher, HttpFetcher, polyhaven::PolyHaven,
    provider::parse_resolution_label,
};
use blender_protocol::asset::{AssetType, DownloadAsset, SearchAssets};

fn provider() -> (PolyHaven, Arc<dyn Fetcher>) {
    let policy = Arc::new(DownloadPolicy::default());
    let fetcher: Arc<dyn Fetcher> =
        Arc::new(HttpFetcher::new(Arc::clone(&policy)).expect("an HTTP client"));
    (PolyHaven::new(Arc::clone(&fetcher)), fetcher)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
}

fn download(provider_id: &str, asset: &str) -> DownloadAsset {
    DownloadAsset {
        provider: provider_id.into(),
        asset_id: asset.into(),
        variant: None,
        resolution: None,
        format: None,
        maps: vec![],
        force: false,
    }
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("blender-mcp-live-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a temp directory");
    root
}

#[test]
#[ignore = "reaches the network"]
fn the_hdri_listing_parses() {
    let (polyhaven, _) = provider();
    let query = SearchAssets {
        query: Some("studio".into()),
        asset_type: Some(AssetType::Hdri),
        ..Default::default()
    };

    let results = runtime()
        .block_on(polyhaven.search(&query))
        .expect("a search");
    assert!(!results.is_empty(), "no HDRIs matched `studio`");

    for asset in &results {
        assert_eq!(asset.asset_type, AssetType::Hdri);
        assert_eq!(asset.provider, "polyhaven");
        assert!(
            !asset.title.is_empty(),
            "{} has no title",
            asset.provider_id
        );
        assert!(!asset.requires_auth);

        let license = asset.license.as_ref().expect("a licence");
        assert_eq!(license.id, "CC0");
        assert_eq!(license.commercial_use, Some(true));
    }

    println!(
        "{} HDRIs matched, first: {}",
        results.len(),
        results[0].title
    );
}

#[test]
#[ignore = "reaches the network"]
fn one_asset_reports_its_real_variants() {
    let (polyhaven, _) = provider();
    let asset = runtime()
        .block_on(polyhaven.get("aarfontein_dirt_road"))
        .expect("the asset");

    assert_eq!(asset.provider_id, "aarfontein_dirt_road");
    assert_eq!(asset.asset_type, AssetType::Hdri);
    assert!(
        asset
            .authors
            .as_ref()
            .is_some_and(|authors| !authors.is_empty()),
        "an asset with no author is a parsing failure, not a real asset"
    );
    assert!(
        !asset.variants.is_empty(),
        "no downloadable variants were found"
    );

    // The resolution ladder must parse: a label this code cannot read would
    // silently narrow what a caller can ask for.
    for variant in &asset.variants {
        assert!(
            parse_resolution_label(&variant.id).is_some(),
            "`{}` is not a resolution label this code understands",
            variant.id
        );
    }
    println!(
        "variants: {:?}",
        asset
            .variants
            .iter()
            .map(|v| v.id.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "reaches the network"]
fn an_hdri_download_plan_matches_the_real_file_listing() {
    let (polyhaven, _) = provider();
    let mut request = download("polyhaven", "aarfontein_dirt_road");
    request.resolution = Some(1024);
    request.format = Some("hdr".into());

    let plan = runtime()
        .block_on(polyhaven.plan(&request))
        .expect("a plan");
    assert_eq!(plan.variant, "1k");
    assert_eq!(plan.files.len(), 1);

    let file = &plan.files[0];
    assert!(
        file.url.starts_with("https://dl.polyhaven.org/"),
        "{}",
        file.url
    );
    assert!(file.filename.ends_with(".hdr"), "{}", file.filename);
    assert!(
        file.size_bytes.is_some_and(|size| size > 0),
        "the provider states a size and it should reach the plan"
    );
    println!("{} ({} bytes)", file.filename, file.size_bytes.unwrap_or(0));
}

#[test]
#[ignore = "reaches the network"]
fn a_texture_set_plans_one_resolution_and_the_standard_maps() {
    let (polyhaven, _) = provider();
    let mut request = download("polyhaven", "rocks_ground_02");
    request.resolution = Some(1024);

    let plan = runtime()
        .block_on(polyhaven.plan(&request))
        .expect("a plan");
    assert_eq!(plan.variant, "1k");
    assert!(
        plan.files.len() >= 3,
        "expected a map set, got {:?}",
        plan.files
    );

    let maps: Vec<&str> = plan.files.iter().filter_map(|f| f.map.as_deref()).collect();
    assert!(maps.contains(&"Diffuse"), "{maps:?}");
    assert!(maps.contains(&"nor_gl"), "{maps:?}");
    assert!(
        !maps.contains(&"nor_dx"),
        "DirectX normals must not be fetched as if they were GL normals: {maps:?}"
    );
    assert!(
        plan.files.iter().all(|file| file.filename.contains("_1k.")),
        "a material must not mix resolutions: {:?}",
        plan.files
            .iter()
            .map(|f| f.filename.as_str())
            .collect::<Vec<_>>()
    );
    println!("maps: {maps:?}");
}

#[test]
#[ignore = "reaches the network"]
fn a_model_plan_keeps_its_texture_paths() {
    let (polyhaven, _) = provider();
    let mut request = download("polyhaven", "CoffeeCart_01");
    request.resolution = Some(1024);
    request.format = Some("blend".into());

    let plan = runtime()
        .block_on(polyhaven.plan(&request))
        .expect("a plan");
    assert!(plan.files[0].filename.ends_with(".blend"));
    assert!(
        plan.files.len() > 1,
        "a model with no textures beside it means `include` was not read"
    );

    let textures: Vec<&str> = plan.files[1..]
        .iter()
        .map(|f| f.filename.as_str())
        .collect();
    assert!(
        textures.iter().all(|name| name.contains('/')),
        "the relative paths the .blend refers to must survive: {textures:?}"
    );

    // Every planned path must pass the policy that will be applied when it is
    // written; a real listing that the policy rejects is a bug in one of them.
    let policy = DownloadPolicy::default();
    for file in &plan.files {
        policy
            .safe_relative_path(&file.filename)
            .unwrap_or_else(|error| panic!("`{}` is rejected: {}", file.filename, error.message));
        policy
            .check_url(&file.url)
            .unwrap_or_else(|error| panic!("`{}` is rejected: {}", file.url, error.message));
    }
    println!("{} files, e.g. {}", plan.files.len(), textures[0]);
}

#[test]
#[ignore = "reaches the network"]
fn a_small_hdri_really_downloads() {
    let root = temp_root("hdri");
    let (polyhaven, fetcher) = provider();
    let policy = Arc::new(DownloadPolicy::default());
    let downloader = Downloader::new(fetcher, policy, root.clone());

    let mut request = download("polyhaven", "aarfontein_dirt_road");
    request.resolution = Some(1024);
    request.format = Some("hdr".into());

    let runtime = runtime();
    let plan = runtime.block_on(polyhaven.plan(&request)).expect("a plan");
    let result = runtime
        .block_on(downloader.run(&plan, None, false))
        .expect("a download");

    assert!(!result.from_cache);
    assert_eq!(result.files.len(), 1);

    let file = &result.files[0];
    let path = root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
    let metadata = std::fs::metadata(&path).expect("the downloaded file");
    assert_eq!(
        metadata.len(),
        file.size_bytes,
        "the recorded size is the real one"
    );
    assert_eq!(file.sha256.len(), 64);
    assert!(
        file.size_bytes > 100_000,
        "a 1k HDR that small is an error page, not an image"
    );

    // Radiance HDR files begin with this signature. Proof that what arrived is
    // the image and not a redirect body or a JSON error.
    let head = std::fs::read(&path).expect("readable");
    assert!(
        head.starts_with(b"#?RADIANCE") || head.starts_with(b"#?RGBE"),
        "the downloaded bytes are not a Radiance HDR"
    );

    // And the second request must not touch the network.
    let cached = runtime
        .block_on(downloader.run(&plan, None, false))
        .expect("a cached result");
    assert!(cached.from_cache);
    assert_eq!(cached.files[0].sha256, file.sha256);

    println!("{} ({} bytes)", file.path, file.size_bytes);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "reaches the network"]
fn an_unknown_asset_is_a_clean_not_found() {
    let (polyhaven, _) = provider();
    let error = runtime()
        .block_on(polyhaven.get("no_such_asset_exists_here_9f2a"))
        .expect_err("this asset does not exist");
    assert_eq!(error.code, blender_protocol::ErrorCode::AssetNotFound);
}
