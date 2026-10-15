//! Arithmetic: `add`, `subtract`, `multiply`, `divide`, `mod`, `pow`, `abs`, `ceil`, `floor`,
//! `exp`, `ln`, `log10`, `sqrt`, `round`, `trunc`, `rand`.
//!
//! Integers stay integers (`divide` truncates, `mod` keeps the dividend's sign, as Java's `/`
//! and `%`) and fail on overflow; with a double, the operation is in doubles. `pow`, `exp`,
//! `ln`, `log10` and `sqrt` give doubles; `exp` and `pow` as Java's `StrictMath` computes them
//! (fdlibm), which differs from the correctly rounded value in the last bit for some arguments.

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
            Ok(Some(value::double(fdlibm_pow(base, exponent))))
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
#[allow(
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    clippy::approx_constant,
    clippy::excessive_precision
)]
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
    } else if hx < 0x3e30_0000 && HUGE + x > 1.0 {
        return 1.0 + x;
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

fn high(x: f64) -> i32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let high = (x.to_bits() >> 32) as i32;
    high
}

fn low(x: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let low = x.to_bits() as u32;
    low
}

fn with_high(x: f64, high: i32) -> f64 {
    #[allow(clippy::cast_sign_loss)]
    let high = u64::from(high as u32);
    f64::from_bits((high << 32) | (x.to_bits() & 0xffff_ffff))
}

fn with_low(x: f64, low: u32) -> f64 {
    f64::from_bits((x.to_bits() & 0xffff_ffff_0000_0000) | u64::from(low))
}

/// `pow` as fdlibm's `__ieee754_pow` (Java's `StrictMath.pow`) computes it.
#[allow(
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::approx_constant,
    clippy::excessive_precision
)]
pub(crate) fn fdlibm_pow(x: f64, y: f64) -> f64 {
    const BP: [f64; 2] = [1.0, 1.5];
    const DP_H: [f64; 2] = [0.0, 5.849_624_872_207_642e-1];
    const DP_L: [f64; 2] = [0.0, 1.350_039_202_129_749e-8];
    const TWO53: f64 = 9_007_199_254_740_992.0;
    const HUGE: f64 = 1.0e300;
    const TINY: f64 = 1.0e-300;
    const L1: f64 = 5.999_999_999_999_946_5e-1;
    const L2: f64 = 4.285_714_285_785_502e-1;
    const L3: f64 = 3.333_333_298_183_774_3e-1;
    const L4: f64 = 2.727_281_238_085_34e-1;
    const L5: f64 = 2.306_607_457_755_617_5e-1;
    const L6: f64 = 2.069_750_178_003_384_2e-1;
    const P1: f64 = 1.666_666_666_666_660_2e-1;
    const P2: f64 = -2.777_777_777_701_559_3e-3;
    const P3: f64 = 6.613_756_321_437_934e-5;
    const P4: f64 = -1.653_390_220_546_525_2e-6;
    const P5: f64 = 4.138_136_797_057_238_5e-8;
    const LG2: f64 = 6.931_471_805_599_453e-1;
    const LG2_H: f64 = 6.931_471_824_645_996e-1;
    const LG2_L: f64 = -1.904_654_299_957_768e-9;
    const OVT: f64 = 8.008_566_259_537_294e-17;
    const CP: f64 = 9.617_966_939_259_756e-1;
    const CP_H: f64 = 9.617_967_009_544_373e-1;
    const CP_L: f64 = -7.028_461_650_952_758e-9;
    const IVLN2: f64 = 1.442_695_040_888_963_4;
    const IVLN2_H: f64 = 1.442_695_021_629_333_5;
    const IVLN2_L: f64 = 1.925_962_991_126_617_5e-8;

    let (hx, lx) = (high(x), low(x));
    let (hy, ly) = (high(y), low(y));
    let mut ix = hx & 0x7fff_ffff;
    let iy = hy & 0x7fff_ffff;

    // y == 0: x**0 = 1.
    if (iy as u32 | ly) == 0 {
        return 1.0;
    }
    // NaN.
    if ix > 0x7ff0_0000
        || (ix == 0x7ff0_0000 && lx != 0)
        || iy > 0x7ff0_0000
        || (iy == 0x7ff0_0000 && ly != 0)
    {
        return x + y;
    }
    // Whether y is an integer when x < 0: 0 not, 1 odd, 2 even.
    let mut yisint = 0;
    if hx < 0 {
        if iy >= 0x4340_0000 {
            yisint = 2;
        } else if iy >= 0x3ff0_0000 {
            let k = (iy >> 20) - 0x3ff;
            if k > 20 {
                let j = ly >> (52 - k);
                if (j << (52 - k)) == ly {
                    yisint = 2 - (j & 1) as i32;
                }
            } else if ly == 0 {
                let j = iy >> (20 - k);
                if (j << (20 - k)) == iy {
                    yisint = 2 - (j & 1);
                }
            }
        }
    }
    // Special values of y.
    if ly == 0 {
        if iy == 0x7ff0_0000 {
            if ((ix - 0x3ff0_0000) as u32 | lx) == 0 {
                return f64::NAN;
            } else if ix >= 0x3ff0_0000 {
                return if hy >= 0 { y } else { 0.0 };
            }
            return if hy < 0 { -y } else { 0.0 };
        }
        if iy == 0x3ff0_0000 {
            return if hy < 0 { 1.0 / x } else { x };
        }
        if hy == 0x4000_0000 {
            return x * x;
        }
        if hy == 0x3fe0_0000 && hx >= 0 {
            return x.sqrt();
        }
    }

    let mut ax = x.abs();
    // Special values of x.
    if lx == 0 && (ix == 0x7ff0_0000 || ix == 0 || ix == 0x3ff0_0000) {
        let mut z = ax;
        if hy < 0 {
            z = 1.0 / z;
        }
        if hx < 0 {
            if ((ix - 0x3ff0_0000) | yisint) == 0 {
                z = f64::NAN;
            } else if yisint == 1 {
                z = -z;
            }
        }
        return z;
    }

    let mut n = (hx >> 31) + 1;
    // (x < 0)**(non-integer) is NaN.
    if (n | yisint) == 0 {
        return f64::NAN;
    }
    let s = if (n | (yisint - 1)) == 0 { -1.0 } else { 1.0 };

    let (t1, t2);
    if iy > 0x41e0_0000 {
        // |y| > 2**31.
        if iy > 0x43f0_0000 {
            if ix <= 0x3fef_ffff {
                return if hy < 0 { HUGE * HUGE } else { TINY * TINY };
            }
            if ix >= 0x3ff0_0000 {
                return if hy > 0 { HUGE * HUGE } else { TINY * TINY };
            }
        }
        if ix < 0x3fef_ffff {
            return if hy < 0 {
                s * HUGE * HUGE
            } else {
                s * TINY * TINY
            };
        }
        if ix > 0x3ff0_0000 {
            return if hy > 0 {
                s * HUGE * HUGE
            } else {
                s * TINY * TINY
            };
        }
        let t = ax - 1.0;
        let w = (t * t) * (0.5 - t * (0.333_333_333_333_333_3 - t * 0.25));
        let u = IVLN2_H * t;
        let v = t * IVLN2_L - w * IVLN2;
        t1 = with_low(u + v, 0);
        t2 = v - (t1 - u);
    } else {
        n = 0;
        if ix < 0x0010_0000 {
            ax *= TWO53;
            n -= 53;
            ix = high(ax);
        }
        n += (ix >> 20) - 0x3ff;
        let j = ix & 0x000f_ffff;
        ix = j | 0x3ff0_0000;
        let k: usize = if j <= 0x3988E {
            0
        } else if j < 0xBB67A {
            1
        } else {
            n += 1;
            ix -= 0x0010_0000;
            0
        };
        ax = with_high(ax, ix);

        let u = ax - BP[k];
        let v = 1.0 / (ax + BP[k]);
        let ss = u * v;
        let s_h = with_low(ss, 0);
        let t_h = with_high(
            0.0,
            ((ix >> 1) | 0x2000_0000) + 0x0008_0000 + ((k as i32) << 18),
        );
        let t_l = ax - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        let mut s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        s2 = s_h * s_h;
        let t_h = with_low(3.0 + s2 + r, 0);
        let t_l = r - ((t_h - 3.0) - s2);
        let u = s_h * t_h;
        let v = s_l * t_h + t_l * ss;
        let p_h = with_low(u + v, 0);
        let p_l = v - (p_h - u);
        let z_h = CP_H * p_h;
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        let t = f64::from(n);
        t1 = with_low(((z_h + z_l) + DP_H[k]) + t, 0);
        t2 = z_l - (((t1 - t) - DP_H[k]) - z_h);
    }

    // (y1 + y2) * (t1 + t2).
    let y1 = with_low(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let z = p_l + p_h;
    let j = high(z);
    let i = low(z) as i32;
    if j >= 0x4090_0000 {
        if ((j - 0x4090_0000) | i) != 0 || p_l + OVT > z - p_h {
            return s * HUGE * HUGE;
        }
    } else if (j & 0x7fff_ffff) >= 0x4090_cc00
        && (((j as u32).wrapping_sub(0xc090_cc00) as i32 | i) != 0 || p_l <= z - p_h)
    {
        return s * TINY * TINY;
    }

    // 2**(p_h + p_l).
    let i = j & 0x7fff_ffff;
    let mut k = (i >> 20) - 0x3ff;
    let mut n = 0;
    if i > 0x3fe0_0000 {
        n = j + (0x0010_0000 >> (k + 1));
        k = ((n & 0x7fff_ffff) >> 20) - 0x3ff;
        let t = with_high(0.0, n & !(0x000f_ffff >> k));
        n = ((n & 0x000f_ffff) | 0x0010_0000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    let t = with_low(p_l + p_h, 0);
    let u = t * LG2_H;
    let v = (p_l - (t - p_h)) * LG2 + t * LG2_L;
    let mut z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1 = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let r = (z * t1) / (t1 - 2.0) - (w + z * w);
    z = 1.0 - (r - z);
    let j = high(z) + (n << 20);
    if (j >> 20) <= 0 {
        // A subnormal result: scale in two steps of exact powers of two.
        let half = n / 2;
        z = z * 2f64.powi(half) * 2f64.powi(n - half);
    } else {
        z = with_high(z, j);
    }
    s * z
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
    fn pow_is_fdlibm() {
        assert_eq!(fdlibm_pow(2.0, 2.5), 5.656_854_249_492_38);
        assert_eq!(fdlibm_pow(2.0, 10.0), 1024.0);
        assert_eq!(fdlibm_pow(2.0, 0.5), std::f64::consts::SQRT_2);
        assert_eq!(fdlibm_pow(2.0, -3.0), 0.125);
        assert_eq!(fdlibm_pow(2.5, 10.0), 9_536.743_164_062_5);
        assert_eq!(fdlibm_pow(1.0, 2.0), 1.0);
        assert_eq!(fdlibm_pow(-2.0, 3.0), -8.0);
        assert!(fdlibm_pow(-2.0, 0.5).is_nan());
        assert!(fdlibm_pow(2.0, f64::NAN).is_nan());
        assert_eq!(fdlibm_pow(10.0, -2.0), 0.01);
        assert_eq!(fdlibm_pow(3.0, 0.3), 3f64.powf(0.3));
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
