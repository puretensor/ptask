//! Black-box tests for `pt flux`: who opened and who closed work.

mod common;
use common::Pt;

#[test]
fn flux_names_who_grew_the_backlog() {
    let pt = Pt::new();
    for t in [
        "triage finding one",
        "triage finding two",
        "triage finding three",
    ] {
        pt.ok_as("hal", &["add", "--raw", t]);
    }
    pt.ok_as("hal", &["done", "PT-1"]);
    pt.ok_as("shell", &["done", "PT-2"]);
    pt.ok_as("shell", &["dismiss", "PT-3"]);
    pt.ok_as("shell", &["reopen", "PT-3"]);

    let r = pt.json(&["flux", "--since", "1h"]);
    assert_eq!(r["window_minutes"], 60);
    let actors = r["actors"].as_array().unwrap();
    assert_eq!(actors[0]["actor"], "hal", "largest net first: {r:#}");
    assert_eq!(actors[0]["created"], 3);
    assert_eq!(actors[0]["done"], 1);
    assert_eq!(actors[0]["net"], 2);
    let shell = actors.iter().find(|a| a["actor"] == "shell").unwrap();
    assert_eq!(
        (
            shell["done"].as_i64(),
            shell["dismissed"].as_i64(),
            shell["reopened"].as_i64(),
            shell["net"].as_i64()
        ),
        (Some(1), Some(1), Some(1), Some(-1))
    );
    assert_eq!(r["total"]["net"], 1);

    let human = pt.ok(&["--no-color", "flux"]);
    assert!(human.contains("hal") && human.contains("+2"), "{human}");
    assert!(
        human.contains("net = created + reopened − done − dismissed"),
        "{human}"
    );

    // The digest carries the same split for its window.
    let digest = pt.json(&["digest"]);
    assert_eq!(digest["flux_by_actor"][0]["actor"], "hal");

    for bad in ["7", "0h", "91d", "soon"] {
        assert!(!pt.run(&["flux", "--since", bad]).status.success(), "{bad}");
    }
}

#[test]
fn an_empty_window_says_so() {
    let pt = Pt::new();
    let human = pt.ok(&["--no-color", "flux", "--since", "30m"]);
    assert!(
        human.contains("no task created, closed or reopened"),
        "{human}"
    );
    assert!(pt.json(&["flux"])["actors"].as_array().unwrap().is_empty());
}
