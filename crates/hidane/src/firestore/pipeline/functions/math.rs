//! Arithmetic: `add`, `subtract`, `multiply`, `divide`, `mod`, `pow`, `abs`, `ceil`, `floor`,
//! `exp`, `ln`, `log10`, `sqrt`, `round`, `trunc`, `rand`.
//!
//! Integers stay integers (`divide` truncates, `mod` keeps the dividend's sign, as Java's `/`
//! and `%`) and fail on overflow; with a double, the operation is in doubles. `pow`, `exp`,
//! `ln`, `log10` and `sqrt` give doubles; `exp` as Java's `StrictMath.exp` computes it (fdlibm),
//! which differs from the correctly rounded value in the last bit for some arguments.

use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{Def, Signature, args, requires};
use crate::{
    firestore::pipeline::{
        expr::{Call, Eval, Scope},
        value,
    },
    rest::java_double_text,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Num {
    Int(i64),
    Double(f64),
}

impl Num {
    pub(crate) fn as_f64(self) -> f64 {
        match self {
            #[allow(clippy::cast_precision_loss)]
            Self::Int(i) => i as f64,
            Self::Double(d) => d,
        }
    }

    pub(crate) fn value(self) -> Value {
        match self {
            Self::Int(i) => value::int(i),
            Self::Double(d) => value::double(d),
        }
    }
}

/// A numeric operand: `None` for null or absent, an error naming `expected` for any other type.
pub(crate) fn number(
    function: &str,
    expected: &str,
    v: Option<&Value>,
) -> Result<Option<Num>, Status> {
    match v.and_then(|v| v.value_type.as_ref()) {
        None | Some(ValueType::NullValue(_)) => Ok(None),
        Some(ValueType::IntegerValue(i)) => Ok(Some(Num::Int(*i))),
        Some(ValueType::DoubleValue(d)) => Ok(Some(Num::Double(*d))),
        Some(_) => Err(requires(function, expected, v.expect("a value"))),
    }
}

/// The operands of a binary function, both checked before a null one makes the result null.
fn operands(call: &Call, scope: &Scope<'_>, name: &str) -> Result<Option<(Num, Num)>, Status> {
    let values = args(call, scope)?;
    let a = number(name, "Number", values[0].as_ref())?;
    let b = number(name, "Number", values[1].as_ref())?;
    Ok(a.zip(b))
}

fn binary(
    call: &Call,
    scope: &Scope<'_>,
    name: &str,
    ints: fn(i64, i64) -> Result<i64, Status>,
    doubles: fn(f64, f64) -> f64,
) -> Eval {
    let Some((a, b)) = operands(call, scope, name)? else {
        return super::null();
    };
    Ok(Some(match (a, b) {
        (Num::Int(a), Num::Int(b)) => value::int(ints(a, b)?),
        _ => value::double(doubles(a.as_f64(), b.as_f64())),
    }))
}

fn invalid(message: &str) -> Status {
    Status::invalid_argument(message)
}

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "add",
        signatures: &[Signature::fixed("add[T <: number](x: T, y: T): T", 2)],
        check: None,
        eval: |call, scope| {
            binary(
                call,
                scope,
                "add",
                |a, b| {
                    a.checked_add(b)
                        .ok_or_else(|| invalid("add(...) int64 overflow"))
                },
                |a, b| a + b,
            )
        },
    },
    Def {
        name: "subtract",
        signatures: &[Signature::fixed("subtract[T <: number](x: T, y: T): T", 2)],
        check: None,
        eval: |call, scope| {
            binary(
                call,
                scope,
                "subtract",
                |a, b| {
                    a.checked_sub(b)
                        .ok_or_else(|| invalid("subtract(...) int64 overflow"))
                },
                |a, b| a - b,
            )
        },
    },
    Def {
        name: "multiply",
        signatures: &[Signature::fixed("multiply[T <: number](x: T, y: T): T", 2)],
        check: None,
        eval: |call, scope| {
            binary(
                call,
                scope,
                "multiply",
                |a, b| {
                    a.checked_mul(b)
                        .ok_or_else(|| invalid("multiply(...) overflow error"))
                },
                |a, b| a * b,
            )
        },
    },
    Def {
        name: "divide",
        signatures: &[Signature::fixed("divide[T <: number](x: T, y: T): T", 2)],
        check: None,
        eval: |call, scope| {
            binary(
                call,
                scope,
                "divide",
                |a, b| {
                    if b == 0 {
                        return Err(invalid("divide(...) requires a non-zero right operand"));
                    }
                    Ok(a.wrapping_div(b))
                },
                |a, b| a / b,
            )
        },
    },
    Def {
        name: "mod",
        signatures: &[Signature::fixed("mod[T <: number](x: T, y: T): T", 2)],
        check: None,
        eval: |call, scope| {
            binary(
                call,
                scope,
                "mod",
                |a, b| {
                    if b == 0 {
                        return Err(invalid("mod(...) requires a non-zero divisor"));
                    }
                    Ok(a.wrapping_rem(b))
                },
                |a, b| a % b,
            )
        },
    },
    Def {
        name: "pow",
        signatures: &[Signature::fixed(
            "pow(base: number, exponent: number): float64",
            2,
        )],
        check: None,
        eval: |call, scope| {
            let Some((base, exponent)) = operands(call, scope, "pow")? else {
                return super::null();
            };
            let (base, exponent) = (base.as_f64(), exponent.as_f64());
            if base == 0.0 && exponent < 0.0 {
                return Err(invalid(
                    "pow(...) the exponent cannot be negative when the base is zero.",
                ));
            }
            if base < 0.0 && exponent.is_finite() && exponent.fract() != 0.0 {
                return Err(invalid(
                    "pow(...) the base cannot be negative when the exponent is not an integer.",
                ));
            }
            Ok(Some(value::double(base.powf(exponent))))
        },
    },
    Def {
        name: "abs",
        signatures: &[Signature::fixed("abs[T <: number](number: T): T", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "abs", "Number", |n| match n {
                Num::Int(i) => i.checked_abs().map(value::int).ok_or_else(|| {
                    invalid("abs(...) cannot be used on minimum Int64 value (-2^63)")
                }),
                Num::Double(d) => Ok(value::double(d.abs())),
            })
        },
    },
    Def {
        name: "ceil",
        signatures: &[Signature::fixed("ceil[T <: number](number: T): T", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "ceil", "Number", |n| {
                Ok(match n {
                    Num::Int(i) => value::int(i),
                    Num::Double(d) => value::double(d.ceil()),
                })
            })
        },
    },
    Def {
        name: "floor",
        signatures: &[Signature::fixed("floor[T <: number](number: T): T", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "floor", "Number", |n| {
                Ok(match n {
                    Num::Int(i) => value::int(i),
                    Num::Double(d) => value::double(d.floor()),
                })
            })
        },
    },
    Def {
        name: "exp",
        signatures: &[Signature::fixed("exp(exponent: number): number", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "exp", "Float", |n| {
                let x = n.as_f64();
                let y = fdlibm_exp(x);
                if y.is_infinite() && x.is_finite() {
                    return Err(invalid("exp(...) exponent overflow"));
                }
                Ok(value::double(y))
            })
        },
    },
    Def {
        name: "ln",
        signatures: &[Signature::fixed("ln(number: number): number", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "ln", "Float", |n| {
                let x = n.as_f64();
                if x.is_nan() || x <= 0.0 {
                    return Err(Status::invalid_argument(format!(
                        "ln(...) requires a positive number but got {}",
                        java_double_text(x)
                    )));
                }
                Ok(value::double(x.ln()))
            })
        },
    },
    Def {
        name: "log10",
        signatures: &[Signature::fixed("log10(number: number): number", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "log10", "Float", |n| {
                let x = n.as_f64();
                if x <= 0.0 {
                    return Err(Status::invalid_argument(format!(
                        "log10(...) requires a positive number but got {}",
                        java_double_text(x)
                    )));
                }
                Ok(value::double(x.log10()))
            })
        },
    },
    Def {
        name: "sqrt",
        signatures: &[Signature::fixed("sqrt(number: number): float64", 1)],
        check: None,
        eval: |call, scope| {
            unary(call, scope, "sqrt", "Float", |n| {
                let x = n.as_f64();
                if x < 0.0 {
                    return Err(invalid("sqrt(...) cannot be used on negative numbers"));
                }
                Ok(value::double(x.sqrt()))
            })
        },
    },
    Def {
        name: "round",
        signatures: &[
            Signature::fixed("round[T <: number](number: T): T", 1),
            Signature::fixed("round[T <: number](number: T, places: int64): T", 2),
        ],
        check: None,
        eval: |call, scope| rounding(call, scope, "round", true),
    },
    Def {
        name: "trunc",
        signatures: &[
            Signature::fixed("trunc[T <: number](number: T): T", 1),
            Signature::fixed("trunc[T <: number](number: T, places: int64): T", 2),
        ],
        check: None,
        eval: |call, scope| rounding(call, scope, "trunc", false),
    },
    Def {
        name: "rand",
        signatures: &[Signature::fixed("rand(): float64", 0)],
        check: None,
        eval: |_, _| Ok(Some(value::double(random_unit()))),
    },
];

/// A random double in [0, 1).
fn random_unit() -> f64 {
    use std::{
        hash::{BuildHasher, Hasher},
        sync::atomic::{AtomicU64, Ordering},
    };
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    #[allow(clippy::cast_precision_loss)]
    let unit = (hasher.finish() >> 11) as f64 / (1_u64 << 53) as f64;
    unit
}

fn unary(
    call: &Call,
    scope: &Scope<'_>,
    name: &str,
    expected: &str,
    f: fn(Num) -> Result<Value, Status>,
) -> Eval {
    let v = scope.eval(&call.args[0])?;
    match number(name, expected, v.as_ref())? {
        Some(n) => f(n).map(Some),
        None => super::null(),
    }
}

/// An integer argument (`places`, a count): an error naming `Int64` for another type, and for a
/// double, Java's message for a lossy conversion.
pub(crate) fn integer(function: &str, v: Option<&Value>) -> Result<Option<i64>, Status> {
    match v.and_then(|v| v.value_type.as_ref()) {
        None | Some(ValueType::NullValue(_)) => Ok(None),
        Some(ValueType::IntegerValue(i)) => Ok(Some(*i)),
        Some(ValueType::DoubleValue(d)) =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            if d.fract() == 0.0 && d.abs() < 9.2e18 {
                Ok(Some(*d as i64))
            } else {
                Err(Status::invalid_argument(format!(
                    "Cannot coerce Float64 ({}) to Int64",
                    java_double_text(*d)
                )))
            }
        }
        Some(_) => Err(requires(function, "Int64", v.expect("a value"))),
    }
}

/// `round` (half away from zero) or `trunc` (toward zero), to `places` decimals.
fn rounding(call: &Call, scope: &Scope<'_>, name: &str, half_up: bool) -> Eval {
    let values = args(call, scope)?;
    let Some(n) = number(name, "Number", values[0].as_ref())? else {
        return super::null();
    };
    let places = match values.get(1) {
        None => 0,
        Some(v) => match integer(name, v.as_ref())? {
            Some(places) => places,
            None => return super::null(),
        },
    };
    Ok(Some(match n {
        Num::Int(i) if places >= 0 => value::int(i),
        Num::Int(i) => {
            let unit = 10_i128.checked_pow(u32::try_from(-places).unwrap_or(u32::MAX));
            let Some(unit) = unit else {
                return Ok(Some(value::int(0)));
            };
            let i = i128::from(i);
            let down = i / unit * unit;
            let rest = (i - down).abs();
            let rounded = if half_up && rest * 2 >= unit {
                down + i.signum() * unit
            } else {
                down
            };
            value::int(i64::try_from(rounded).unwrap_or(i64::MAX))
        }
        Num::Double(d) if !d.is_finite() => value::double(d),
        Num::Double(d) if places == 0 => value::double(if half_up { d.round() } else { d.trunc() }),
        Num::Double(d) => value::double(decimal_round(d, places, half_up)),
    }))
}

/// `d` rounded to `places` decimals as `BigDecimal.valueOf(d).setScale(places, HALF_UP or
/// DOWN)` does it: on the shortest decimal that reads back as `d`.
fn decimal_round(d: f64, places: i64, half_up: bool) -> f64 {
    if d == 0.0 {
        return d;
    }
    let (digits, exponent) = crate::rest::shortest(d.abs());
    // `digits` is d.ddd × 10^exponent: the digit at index k has the place value
    // 10^(exponent - k). Keep those of place value ≥ 10^-places.
    let keep = i64::from(exponent) + places + 1;
    let digits = digits.as_bytes();
    let Ok(keep) = usize::try_from(keep) else {
        // Every digit goes: 0, or one unit of the last place kept when rounding up from ≥ .5.
        let round_up = half_up && keep == 0 && digits[0] >= b'5';
        let magnitude = if round_up {
            format!("1e{}", -places)
        } else {
            "0".to_owned()
        };
        return signed(d, &magnitude);
    };
    if keep >= digits.len() {
        return d;
    }
    let mut kept: Vec<u8> = digits[..keep].to_vec();
    let mut exponent = i64::from(exponent);
    if half_up && digits[keep] >= b'5' {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                exponent += 1;
                break;
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    if kept.is_empty() {
        return signed(d, "0");
    }
    let mantissa = String::from_utf8(kept).expect("digits");
    signed(d, &format!("0.{mantissa}e{}", exponent + 1))
}

fn signed(d: f64, magnitude: &str) -> f64 {
    let m: f64 = magnitude.parse().unwrap_or(0.0);
    if d < 0.0 { -m } else { m }
}

/// `exp` as fdlibm's `__ieee754_exp` (Java's `StrictMath.exp`) computes it.
#[allow(clippy::many_single_char_names, clippy::unreadable_literal)]
pub(crate) fn fdlibm_exp(x: f64) -> f64 {
    const HALF: [f64; 2] = [0.5, -0.5];
    const HUGE: f64 = 1.0e+300;
    const TWOM1000: f64 = 9.332_636_185_032_189e-302;
    const O_THRESHOLD: f64 = 7.097_827_128_933_84e2;
    const U_THRESHOLD: f64 = -7.451_332_191_019_411e2;
    const LN2_HI: [f64; 2] = [6.931_471_803_691_238e-1, -6.931_471_803_691_238e-1];
    const LN2_LO: [f64; 2] = [1.908_214_929_270_587_7e-10, -1.908_214_929_270_587_7e-10];
    const INVLN2: f64 = 1.442_695_040_888_963_4;
    const P1: f64 = 1.666_666_666_666_660_2e-1;
    const P2: f64 = -2.777_777_777_701_559_3e-3;
    const P3: f64 = 6.613_756_321_437_934e-5;
    const P4: f64 = -1.653_390_220_546_525_2e-6;
    const P5: f64 = 4.138_136_797_057_238_5e-8;

    let bits = x.to_bits();
    #[allow(clippy::cast_possible_truncation)]
    let mut hx = (bits >> 32) as u32;
    let xsb = ((hx >> 31) & 1) as usize;
    hx &= 0x7fff_ffff;

    if hx >= 0x4086_2E42 {
        if hx >= 0x7ff0_0000 {
            #[allow(clippy::cast_possible_truncation)]
            let lo = bits as u32;
            if ((hx & 0xfffff) | lo) != 0 {
                return x + x;
            }
            return if xsb == 0 { x } else { 0.0 };
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE;
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000;
        }
    }

    let mut x = x;
    let mut hi = 0.0;
    let mut lo = 0.0;
    let mut k: i32 = 0;
    if hx > 0x3fd6_2e42 {
        if hx < 0x3FF0_A2B2 {
            hi = x - LN2_HI[xsb];
            lo = LN2_LO[xsb];
            k = 1 - 2 * i32::try_from(xsb).unwrap_or(0);
        } else {
            #[allow(clippy::cast_possible_truncation)]
            {
                k = (INVLN2 * x + HALF[xsb]) as i32;
            }
            let t = f64::from(k);
            hi = x - t * LN2_HI[0];
            lo = t * LN2_LO[0];
        }
        x = hi - lo;
    } else if hx < 0x3e30_0000 {
        if HUGE + x > 1.0 {
            return 1.0 + x;
        }
    }

    let t = x * x;
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo - (x * c) / (2.0 - c)) - hi);
    let add_exponent = |y: f64, k: i32| {
        let bits = y.to_bits();
        #[allow(clippy::cast_sign_loss)]
        let high = ((bits >> 32) as i64 + (i64::from(k) << 20)) as u64;
        f64::from_bits((high << 32) | (bits & 0xffff_ffff))
    };
    if k >= -1021 {
        add_exponent(y, k)
    } else {
        add_exponent(y, k + 1000) * TWOM1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_is_fdlibm() {
        assert_eq!(fdlibm_exp(1.0), 2.718_281_828_459_045_5);
        assert_eq!(fdlibm_exp(0.0), 1.0);
        assert_eq!(fdlibm_exp(2.5), 12.182_493_960_703_473);
        assert!(fdlibm_exp(f64::NAN).is_nan());
        assert_eq!(fdlibm_exp(f64::NEG_INFINITY), 0.0);
        assert!(fdlibm_exp(1000.0).is_infinite());
    }

    #[test]
    fn rounds_on_the_shortest_decimal() {
        assert_eq!(decimal_round(1234.5678, 2, true), 1234.57);
        assert_eq!(decimal_round(1234.5678, 2, false), 1234.56);
        assert_eq!(decimal_round(1234.5678, -2, true), 1200.0);
        assert_eq!(decimal_round(2.5, 1, true), 2.5);
        assert_eq!(decimal_round(0.125, 2, true), 0.13);
        assert_eq!(decimal_round(-0.125, 2, true), -0.13);
        assert_eq!(decimal_round(9.99, 1, true), 10.0);
        assert_eq!(decimal_round(4.0, -1, true), 0.0);
        assert_eq!(decimal_round(6.0, -1, true), 10.0);
    }
}
