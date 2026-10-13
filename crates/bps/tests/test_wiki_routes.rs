//! The wiki route scraper (`scripts/scrape_wiki_routes.py` in the Python tree). The Python
//! side had no test for the table reader; its three regexes became hand-written scanners, so
//! these pin the behavior the regexes had: IGNORECASE tags, a gap between the id cells and
//! the name cell, rows without a name skipped, and nothing read before the first heading.

use bps::wiki_routes::{RouteRow, emit_bps, packet_name, parse_tables};
use std::collections::BTreeSet;

fn row(state: &str, direction: &str, packet_id: u64, official_name: &str) -> RouteRow {
    RouteRow {
        state: state.into(),
        direction: direction.into(),
        packet_id,
        official_name: official_name.into(),
    }
}

const PAGE: &str = r#"
<table><tr><td>9</td><td>0x09</td><td>Stray</td><td><code>before_any_heading</code></td></tr></table>
<table>
<tr><th colspan="4">Play clientbound packets</th></tr>
<tr><th>Packet ID</th><th>State</th><th>Bound To</th><th>Resource</th></tr>
<TR class="x">
<TD>0</TD>
<td>0x00
</td><td>Play</td><td>Client</td><td><CODE>bundle_delimiter</CODE></td></TR>
<tr><td>1</td><td>0x01</td><td>Play</td><td><code> add_entity </code></td></tr>
<tr><td>2</td><td>0x02</td><td>Play</td><td>no code cell here</td></tr>
</table>
<table>
<TH COLSPAN="4">LOGIN ServerBound PACKETS</TH>
<tr><td>0</td><td>0x00</td><td>Login</td><td><code>hello</code></td></tr>
<tr><td>17</td><td>0x11</td><td>Login</td><td><code>custom_query_answer</code></td></tr>
</table>
"#;

#[test]
fn each_row_belongs_to_the_heading_above_it() {
    assert_eq!(
        parse_tables(PAGE),
        vec![
            row("play", "clientbound", 0, "bundle_delimiter"),
            row("play", "clientbound", 1, "add_entity"),
            row("login", "serverbound", 0, "hello"),
            row("login", "serverbound", 17, "custom_query_answer"),
        ]
    );
}

#[test]
fn a_page_without_headings_has_no_routes() {
    assert!(
        parse_tables("<tr><td>1</td><td>0x01</td><td>x</td><td><code>y</code></td></tr>").is_empty()
    );
}

#[test]
fn official_names_map_to_the_hand_written_packets() {
    let cases = [
        // (official, state, direction, id) -> BPS packet
        (("intention", "handshaking", "serverbound", 0), "handshake"),
        (("intention", "play", "clientbound", 1), "handshake"),
        (("hello", "login", "serverbound", 0), "login_start"),
        (("key", "login", "serverbound", 1), "encryption_response"),
        (("hello", "login", "clientbound", 0), "hello"),
        (("change_difficulty", "play", "serverbound", 2), "change_difficulty_serverbound"),
        (("container_close", "play", "serverbound", 0x12), "container_close_serverbound"),
        (("move_vehicle", "play", "serverbound", 0x20), "move_vehicle_serverbound"),
        (("player_abilities", "play", "serverbound", 0x27), "player_abilities_serverbound"),
        (("show_dialog", "configuration", "clientbound", 0x12), "show_dialog_configuration"),
        // The dialog alias is configuration-only: play keeps the plain name.
        (("show_dialog", "play", "clientbound", 0x7e), "show_dialog"),
        (("some-name", "play", "clientbound", 5), "some_name"),
        (("1.foo", "play", "clientbound", 0x0a), "pkt_play_clientbound_0a"),
        (("", "status", "clientbound", 3), "pkt_status_clientbound_03"),
    ];
    for ((official, state, direction, id), want) in cases {
        assert_eq!(
            packet_name(official, state, direction, id),
            want,
            "{official:?} in {state}.{direction}"
        );
    }
}

#[test]
fn the_route_file_declares_a_stub_only_for_packets_no_file_owns() {
    let rows = [
        row("handshaking", "serverbound", 0, "intention"),
        row("play", "clientbound", 0x0a, "some-name"),
        row("play", "clientbound", 1, "alpha"),
    ];
    let extra = ["    route handshaking.serverbound 0xfe => legacy_server_list_ping".to_string()];
    // `alpha` is declared by a hand-written file; `handshake` is one of the fixed hand packets.
    let manual = BTreeSet::from(["alpha".to_string()]);
    let imports = ["std/varint.bps".to_string(), "packets.bps".to_string()];
    let want = "\
// Packet stubs + routes transcribed from minecraft.wiki (protocol 777 / 26.3).
// Regenerate: cargo run -q -p bps --bin scrape_wiki_routes -- -o schemas/minecraft_26_3_wiki.bps

import \"std/varint.bps\"
import \"packets.bps\"

packet some_name {
    body: rest
}

protocol minecraft @ v26_3 {
    id = 777
    frame default {
        length: varint
        id: varint
        payload: rest
    }

    route handshaking.serverbound 0x00 => handshake
    route handshaking.serverbound 0xfe => legacy_server_list_ping
    route play.clientbound 0x01 => alpha
    route play.clientbound 0x0a => some_name
}
";
    assert_eq!(emit_bps(&rows, &extra, &manual, &imports), want);
}

#[test]
fn an_unknown_flag_is_a_usage_error() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_scrape_wiki_routes"))
        .arg("--bogus")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unrecognized arguments: --bogus"), "{stderr}");
}
