use crate::base::scalar::{Curve25519Scalar, Scalar};
use ark_ff::BigInt;
use num_traits::{Inv, One};

#[test]
fn test_dalek_interop_1() {
    let x = curve25519_dalek::scalar::Scalar::from(1u64);
    let xp = Curve25519Scalar::from(1u64);
    assert_eq!(curve25519_dalek::scalar::Scalar::from(xp), x);
}

#[test]
fn test_dalek_interop_m1() {
    let x = curve25519_dalek::scalar::Scalar::from(123u64);
    let mx = -x;
    let xp = Curve25519Scalar::from(123u64);
    let mxp = -xp;
    assert_eq!(mxp, Curve25519Scalar::from(-123i64));
    assert_eq!(curve25519_dalek::scalar::Scalar::from(mxp), mx);
}

#[test]
fn test_add() {
    let one = Curve25519Scalar::from(1u64);
    let two = Curve25519Scalar::from(2u64);
    let sum = one + two;
    let expected_sum = Curve25519Scalar::from(3u64);
    assert_eq!(sum, expected_sum);
}

#[test]
fn test_mod() {
    let pm1: BigInt<4> =
        BigInt!("7237005577332262213973186563042994240857116359379907606001950938285454250988");
    let x = Curve25519Scalar::from(pm1.0);
    let one = Curve25519Scalar::from(1u64);
    let zero = Curve25519Scalar::from(0u64);
    let xp1 = x + one;
    assert_eq!(xp1, zero);
}

#[test]
fn test_curve25519_scalar_serialization() {
    let s = [
        Curve25519Scalar::from(1u8),
        -Curve25519Scalar::from(1u8),
        Curve25519Scalar::from(123),
        Curve25519Scalar::from(0),
        Curve25519Scalar::from(255),
        Curve25519Scalar::from(1234),
        Curve25519Scalar::from(12345),
        Curve25519Scalar::from(2357),
        Curve25519Scalar::from(999),
        Curve25519Scalar::from(123456789),
    ];
    let serialized = serde_json::to_string(&s).unwrap();
    let deserialized: [Curve25519Scalar; 10] = serde_json::from_str(&serialized).unwrap();
    assert_eq!(s, deserialized);
}

#[test]
fn test_curve25519_scalar_display() {
    assert_eq!(
        "0000000000000000000000000000000000000000000000000000000000ABC123",
        format!("{}", Curve25519Scalar::from(0xABC123))
    );
    assert_eq!(
        "1000000000000000000000000000000014DEF9DEA2F79CD65812631A5C4A12CA",
        format!("{}", Curve25519Scalar::from(-0xABC123))
    );
    assert_eq!(
        "0x0000...C123",
        format!("{:#}", Curve25519Scalar::from(0xABC123))
    );
    assert_eq!(
        "0x1000...12CA",
        format!("{:#}", Curve25519Scalar::from(-0xABC123))
    );
    assert_eq!(
        "+0000000000000000000000000000000000000000000000000000000000ABC123",
        format!("{:+}", Curve25519Scalar::from(0xABC123))
    );
    assert_eq!(
        "-0000000000000000000000000000000000000000000000000000000000ABC123",
        format!("{:+}", Curve25519Scalar::from(-0xABC123))
    );
    assert_eq!(
        "+0x0000...C123",
        format!("{:+#}", Curve25519Scalar::from(0xABC123))
    );
    assert_eq!(
        "-0x0000...C123",
        format!("{:+#}", Curve25519Scalar::from(-0xABC123))
    );
}

#[test]
fn test_curve25519_scalar_mid() {
    assert_eq!(
        Curve25519Scalar::MAX_SIGNED,
        -Curve25519Scalar::one() * Curve25519Scalar::from(2).inv().unwrap()
    );
}