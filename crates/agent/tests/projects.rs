//! Project detection on the synthetic fixture: project-NNN folders, the
//! Downloads sessions, and name persistence across rebuilds.
#![cfg(unix)]

use filemind_agent::{analysis, fixture, pipeline};
use filemind_storage::Db;

#[test]
fn detects_fixture_projects_and_keeps_user_names() {
    let adapter = filemind_adapter_macos::MacAdapter;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("fx");
    fixture::build(&root, 2000).unwrap();
    let root = root.canonicalize().unwrap();
    let db = Db::open_in_memory().unwrap();
    pipeline::scan_root(&adapter, &db, &root).unwrap();
    let a = analysis::run(&db).unwrap();
    assert!(a.projects > 0);

    let ps = db.list_projects(1000).unwrap();
    // projects/ is a container of project-NNN folders → each becomes its own project;
    // Downloads/ and versions/ are folder projects
    let names: Vec<&str> = ps.iter().map(|p| p.display_name()).collect();
    assert!(
        !names.contains(&"Projects"),
        "container must not be a project: {names:?}"
    );
    assert!(
        names.iter().filter(|n| n.starts_with("Project 0")).count() >= 30,
        "{names:?}"
    );
    assert!(names.contains(&"Downloads"), "{names:?}");
    assert!(names.contains(&"Versions"), "{names:?}");
    // every member file belongs to exactly one project
    let dup: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) - COUNT(DISTINCT file_id) FROM project_files",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dup, 0);

    // rename survives a rebuild
    let id = ps
        .iter()
        .find(|p| p.display_name() == "Versions")
        .unwrap()
        .project_id;
    assert!(db.rename_project(id, Some("Quarterly reports")).unwrap());
    analysis::run(&db).unwrap();
    let again = db.project(id).unwrap().unwrap();
    assert_eq!(again.display_name(), "Quarterly reports");
    assert_eq!(again.suggested_name, "Versions");

    // lookup by path
    let some = db.project_files(id, 1).unwrap();
    let pr = db.project_of_path(&some[0].0).unwrap().unwrap();
    assert_eq!(pr.project_id, id);
}
