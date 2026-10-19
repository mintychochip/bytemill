//! `bytemill::json::float_repr` is Python's `repr(float)`, which `decode`'s JSON output and every
//! ledger/pin file print floats with. The expected strings are what Python prints. The tie cases
//! (a double exactly halfway between two decimals of the shortest length) are the ones Rust's own
//! formatting gets wrong: Python takes the even last digit, Rust the upper candidate.

use bytemill::json::float_repr;

// The tie inputs are written with every digit of the exact value on purpose.
#[allow(clippy::excessive_precision)]
#[test]
fn floats_print_as_python_repr_does() {
    let cases: [(f64, &str); 17] = [
        // Exact ties, from decoded `f32be` fields (every f32 is an exact double).
        (0.83985137939453125, "0.8398513793945312"),
        (0.50391387939453125, "0.5039138793945312"),
        (1.0 + 1.0 / 131072.0, "1.0000076293945312"),
        (-(1.0 + 1.0 / 131072.0), "-1.0000076293945312"),
        // Exactly representable in 17 digits: the shortest form is the exact value.
        (3.0 + 1.0 / 65536.0, "3.0000152587890625"),
        (3.0 / 1048576.0, "2.86102294921875e-06"),
        // Ordinary shortest round-trip, and the fixed/scientific switch.
        (0.1, "0.1"),
        (0.30000000000000004, "0.30000000000000004"),
        (2.5, "2.5"),
        (100.0, "100.0"),
        (1.5e-5, "1.5e-05"),
        (1e16, "1e+16"),
        (1e22, "1e+22"),
        (123456789012345680.0, "1.2345678901234568e+17"),
        (9007199254740993.0, "9007199254740992.0"),
        (5e-324, "5e-324"),
        (f64::MAX, "1.7976931348623157e+308"),
    ];
    for (value, want) in cases {
        assert_eq!(float_repr(value), want, "{value:e}");
    }
}
