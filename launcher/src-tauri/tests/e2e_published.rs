//! Installs a tree written by `release.py publish --out DIR` into a fake game folder: checks that the Python
//! publisher and this launcher agree on the format. Skipped unless DZ_PUB (the folder) is set; the key is
//! launcher/release_key.json, so the tree must be signed with the real launcher key, or DZ_PUB_KEY names the public
//! key (hex) of a test key it was signed with.
//!   DZ_PUB=C:\path\to\pub cargo test --test e2e_published -- --nocapture

use dz_launcher_lib::core::install::{installed, list_backups, make_plan, no_progress, update, wanted, Layout};
use dz_launcher_lib::core::manifest::ReleaseKey;
use dz_launcher_lib::core::source::Source;
use std::collections::BTreeMap;

#[tokio::test]
async fn installs_a_python_published_tree() {
    let Ok(dir) = std::env::var("DZ_PUB") else {
        eprintln!("DZ_PUB not set: skipped");
        return;
    };
    let src = Source::parse(&dir).unwrap();
    let key = std::env::var("DZ_PUB_KEY").map(|k| ReleaseKey::from_hex(&k).unwrap()).unwrap_or_else(|_| ReleaseKey::builtin());
    let m = src.manifest(&key, "stable").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let layout = Layout::new(tmp.path().join("_anniversary_"));
    std::fs::create_dir_all(layout.addons()).unwrap();
    // every game, as a member who keeps them all
    let all: BTreeMap<String, bool> = m.packs.iter().map(|p| (p.id.clone(), true)).collect();
    let want = wanted(&layout, &m, &all);
    let t = std::time::Instant::now();
    let out = update(&layout, &src, &m, &want, 3, no_progress()).await.unwrap();
    eprintln!("installed {} ({} files, {} bytes) in {:?}", out.version, out.changed, out.downloaded, t.elapsed());
    let i = installed(&layout);
    assert_eq!(i.version.as_deref(), Some(m.version.as_str()));
    assert_eq!(i.build.as_deref(), Some(m.build.as_str()));
    assert_eq!(i.asset_pack, Some(m.asset_pack));
    assert!(i.rooms && !i.dev);
    for p in m.packs.iter().filter(|p| p.kind != "core") {
        assert!(i.packs.iter().any(|q| q.id == p.id && q.folder == p.folder), "{} is not installed", p.folder);
    }
    let t = std::time::Instant::now();
    assert!(make_plan(&layout, &m, &want).unwrap().up_to_date);
    eprintln!("up-to-date check: {:?}", t.elapsed());
    assert!(list_backups(&layout).is_empty());
}
