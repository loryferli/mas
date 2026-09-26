//! The committed Lyon study area: the rules `data/lyon/README.md` states, checked on the files.
//!
//! Nothing here reaches `scripts/fetch-lyon.py` or the network. What is checked is that the
//! extract obeys the selection it claims, so a hand edit or a changed rule that forgot the README
//! fails here rather than in a figure.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn flows() -> Vec<csv::StringRecord> {
    let mut reader = csv::Reader::from_path(repo("data/lyon/flows.csv")).expect("flows.csv opens");
    let header = reader.headers().expect("a header").clone();
    assert_eq!(
        header.iter().collect::<Vec<_>>(),
        [
            "rank",
            "origin_code",
            "origin_name",
            "origin_latitude",
            "origin_longitude",
            "destination_code",
            "destination_name",
            "destination_latitude",
            "destination_longitude",
            "distance_km",
            "motorised_flow",
        ]
    );
    reader.records().map(|row| row.expect("a row")).collect()
}

fn number(row: &csv::StringRecord, column: usize) -> f64 {
    row[column].parse().expect("a number")
}

#[test]
fn the_demand_is_the_thousand_largest_qualifying_pairs_largest_first() {
    let rows = flows();
    assert_eq!(rows.len(), 1000);
    let mut previous = f64::INFINITY;
    let mut seen = HashSet::new();
    for (rank, row) in rows.iter().enumerate() {
        assert_eq!(row[0].parse::<usize>().unwrap(), rank);
        for code in [&row[1], &row[5]] {
            assert!(code.starts_with("69"), "{code} is outside the Rhône");
        }
        assert_ne!(&row[1], &row[5], "a pair from a place to itself");
        assert!(
            seen.insert((row[1].to_string(), row[5].to_string())),
            "a pair twice"
        );
        let distance_km = number(row, 9);
        assert!(
            (5.0..=50.0).contains(&distance_km),
            "{distance_km} km is outside 5-50 km"
        );
        let flow = number(row, 10);
        assert!(flow >= 10.0, "a flow of {flow} is below the floor");
        assert!(flow <= previous, "rank {rank} is out of order");
        previous = flow;
        for (latitude, longitude) in [
            (number(row, 3), number(row, 4)),
            (number(row, 7), number(row, 8)),
        ] {
            assert!(
                (45.4..=46.4).contains(&latitude) && (4.2..=5.2).contains(&longitude),
                "{latitude},{longitude} is not in the Rhône"
            );
        }
    }
}

#[test]
fn lyon_residents_are_placed_in_their_arrondissement() {
    // 69123 is Lyon as a whole; the rule places both ends in one of its nine arrondissements.
    for row in flows() {
        assert!(
            &row[1] != "69123" && &row[5] != "69123",
            "{:?} names Lyon as a whole",
            row
        );
    }
}

#[test]
fn every_station_the_scenarios_name_is_committed() {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(repo("data/lyon/stations.json")).expect("stations.json reads"),
    )
    .expect("stations.json parses");
    let names: HashSet<&str> = document["stations"]
        .as_array()
        .expect("a station list")
        .iter()
        .map(|station| station["name"].as_str().expect("a name"))
        .collect();
    // The seventeen-station network, both variants, and the lane's two stops among them.
    for name in [
        "Villefranche-sur-Saone",
        "Anse",
        "Limonest",
        "Lentilly",
        "Fleurieux",
        "Ecully",
        "Caluire",
        "Villeurbanne",
        "Meyzieu",
        "Mermoz",
        "Saint-Laurent-de-Mure",
        "Bourgoin",
        "Oullins",
        "Brignais",
        "Mornant",
        "Francheville",
        "Craponne",
        "Belleville",
        "Saint-Priest",
    ] {
        assert!(names.contains(name), "{name} is missing from stations.json");
    }
    assert_eq!(names.len(), 19);
    let places: HashSet<&str> = document["places"]
        .as_array()
        .expect("a place list")
        .iter()
        .map(|place| place["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(places, HashSet::from(["Bourgoin-Jallieu", "Lyon 8e"]));
}
