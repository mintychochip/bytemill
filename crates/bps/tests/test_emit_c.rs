//! `emit c` produces a unit `cc` can round-trip.

fn repo(rel: &str) -> String {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn c_varint_roundtrip() {
    let resolved = bps::analyze(&bps::parse(&repo("schemas/std/varint.bps")).unwrap()).unwrap();
    let dir = std::env::temp_dir().join(format!("bps-c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bps::emit_c::emit_c(&resolved, &dir).unwrap();
    std::fs::write(
        dir.join("check.c"),
        r#"#include "bpsgen.h"
#include <stdio.h>
#include <string.h>
static int fail;
static void expect(const char *name, int64_t value, const uint8_t *want, size_t n) {
    uint8_t buf[16];
    bps_writer w = {buf, 0, sizeof buf};
    if (bps_encode_varint(&w, 0, value)) { printf("encode %s failed\n", name); fail = 1; return; }
    if (w.len != n || memcmp(buf, want, n) != 0) {
        printf("encode %s bytes\n", name); fail = 1; return;
    }
    bps_cursor c = {buf, w.len, 0};
    int64_t got = 0;
    if (bps_decode_varint(&c, 0, &got) || got != value) {
        printf("decode %s got %lld\n", name, (long long)got); fail = 1;
    }
}
int main(void) {
    const uint8_t a[] = {0xac, 0x02};
    const uint8_t b[] = {0xff, 0xff, 0xff, 0xff, 0x0f};
    expect("300", 300, a, 2);
    expect("-1", -1, b, 5);
    return fail;
}
"#,
    )
    .unwrap();
    let bin = dir.join("check");
    let compile = std::process::Command::new("cc")
        .args(["-Wall", "-Werror", "-o"])
        .arg(&bin)
        .arg(dir.join("check.c"))
        .arg(dir.join("bpsgen.c"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "cc failed\n{}\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = std::process::Command::new(&bin).output().unwrap();
    assert!(
        run.status.success(),
        "c check failed\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
