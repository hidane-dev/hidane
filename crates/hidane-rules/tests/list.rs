//! Lists, decided from the query's filter as the official emulator decides them: a few cases of
//! `tools/oracle/rules_list.py`, whose fixture the server's tests replay in full.

mod common;

use common::{Store, document_path, now};
use hidane_rules::{
    SourceFile, compile,
    eval::{
        Budget, Decision, Document, Filter, FilterOp, Map, Method, Query, Request, Value, authorize,
    },
};

fn field(name: &str, op: FilterOp, value: Value) -> Filter {
    Filter::Field {
        field: name.split('.').map(String::from).collect(),
        op,
        value,
    }
}

/// The decision on a list of `c` (or of the collection group `c`) under `allow list: if
/// <condition>` in `match` (`/{path=**}/c/{d}` by default).
fn list_in(matched: &str, condition: &str, filter: Option<Filter>, group: bool) -> Decision {
    let content = format!(
        "rules_version = '2';\nservice cloud.firestore {{\n  match /databases/{{database}}/documents {{\n    \
         match {matched} {{\n      allow list: if {condition};\n    }}\n  }}\n}}\n"
    );
    let ruleset = compile(&[SourceFile {
        name: None,
        content,
    }])
    .ruleset
    .expect("compiles");
    let mut path = document_path("c");
    if group {
        path.pop();
    }
    let request = Request {
        method: Method::List,
        upsert: false,
        in_transaction: false,
        write_fields: None,
        transforms: None,
        auto_id: false,
        verify: false,
        path,
        auth: None,
        time: now(),
        resource: None,
        query: Some(Query {
            offset: Some(0),
            filter,
            group: group.then(|| "c".to_owned()),
            ..Query::default()
        }),
    };
    let stored = Document {
        path: document_path("c/d1"),
        data: Map::from([("n".to_owned(), Value::Int(1))]),
    };
    authorize(
        &ruleset,
        &request,
        None,
        &Store(vec![stored]),
        &mut Budget::query(),
    )
}

fn list(condition: &str, filter: Filter) -> Decision {
    list_in("/{path=**}/c/{d}", condition, Some(filter), false)
}

fn denied(message: &str) -> Decision {
    Decision::Deny(message.to_owned())
}

const UNDECIDED: &str = "\nevaluation error at L5:22 for 'list' @ L5";

#[test]
fn a_field_with_one_equality_holds_its_value() {
    let one = || field("n", FilterOp::Eq, Value::Int(1));
    assert_eq!(list("resource.data.n == 1", one()), Decision::Allow);
    assert_eq!(list("resource.data.n + 1 == 2", one()), Decision::Allow);
    assert_eq!(list("resource.data.n is int", one()), Decision::Allow);
    assert_eq!(
        list("resource.data.n == 2", one()),
        denied("\nfalse for 'list' @ L5")
    );
    assert_eq!(
        list("resource.data.s == 'x'", one()),
        denied("\nProperty s is undefined on object. for 'list' @ L5")
    );
    assert_eq!(
        list("resource == null", one()),
        denied("\nfalse for 'list' @ L5")
    );
}

#[test]
fn a_constraint_implies_what_one_of_its_filters_implies() {
    let above = |v| field("n", FilterOp::Gt, v);
    for allowed in [
        "resource.data.n > 4",
        "resource.data.n > 4.5",
        "resource.data.n >= 5",
        "resource.data.n != 5",
        "resource.data.n != null",
        "5 < resource.data.n",
        "resource.data.n is constraint",
        "resource.data.n == 1 || resource.data.n > 0",
    ] {
        assert_eq!(
            list(allowed, above(Value::Int(5))),
            Decision::Allow,
            "{allowed}"
        );
    }
    for undecided in [
        "resource.data.n > 6",
        "resource.data.n < 3",
        "resource.data.n == 6",
        "!(resource.data.n < 3)",
        "resource.data.n != 'a'",
        "resource.data.n == null",
    ] {
        assert_eq!(
            list(undecided, above(Value::Int(5))),
            denied(UNDECIDED),
            "{undecided}"
        );
    }
    assert_eq!(
        list("resource.data.n in [6, 7]", above(Value::Int(5))),
        denied("\nfalse for 'list' @ L5")
    );
    assert_eq!(
        list("resource.data.n + 1 > 6", above(Value::Int(5))),
        denied(
            "\nUnsupported operation error. Received: constraint + int. Expected: int + int, \
             float + float, string + string, duration + duration, duration + timestamp, \
             timestamp + duration. for 'list' @ L5"
        )
    );
    // A float in the filter does not compare with an int in the rule.
    assert_eq!(
        list("resource.data.n > 1", above(Value::Float(1.5))),
        denied(UNDECIDED)
    );
    assert_eq!(
        list("resource.data.n > 1.0", above(Value::Float(1.5))),
        Decision::Allow
    );
    // Two bounds do not pin a value down.
    let one_to_one = Filter::And(vec![
        field("n", FilterOp::Ge, Value::Int(1)),
        field("n", FilterOp::Le, Value::Int(1)),
    ]);
    assert_eq!(
        list("resource.data.n == 1", one_to_one.clone()),
        denied(UNDECIDED)
    );
    assert_eq!(list("resource.data.n != 3", one_to_one), Decision::Allow);
}

#[test]
fn filters_with_null_and_nan() {
    let n = |op, v| field("n", op, v);
    let unary = |op| field("n", op, Value::Null);
    let nan = || Value::Float(f64::NAN);
    // A comparison with null or NaN matches nothing: `EQ true, EQ false`, of which `EQ true`
    // implies `== true` and `EQ false` `== false`.
    for nothing in [
        n(FilterOp::Ne, Value::Null),
        n(FilterOp::Eq, Value::Null),
        n(FilterOp::Gt, Value::Null),
        n(FilterOp::Eq, nan()),
        n(FilterOp::In, Value::List(vec![Value::Null])),
        n(FilterOp::NotIn, Value::List(vec![Value::Null])),
    ] {
        for allowed in [
            "resource.data.n == true && resource.data.n == false",
            "resource.data.n != true",
            "resource.data.n is constraint",
        ] {
            assert_eq!(
                list(allowed, nothing.clone()),
                Decision::Allow,
                "{allowed}: {nothing:?}"
            );
        }
        for undecided in ["resource.data.n == null", "resource.data.n != 1"] {
            assert_eq!(
                list(undecided, nothing.clone()),
                denied(UNDECIDED),
                "{undecided}: {nothing:?}"
            );
        }
    }
    // `!= NaN` matches every value but null, as `is not null` does.
    for not_null in [n(FilterOp::Ne, nan()), unary(FilterOp::IsNotNull)] {
        assert_eq!(
            list("resource.data.n != null", not_null.clone()),
            Decision::Allow
        );
        assert_eq!(
            list("resource.data.n != float('NaN')", not_null),
            denied(UNDECIDED)
        );
    }
    // No number is NaN: every filter with another number implies `!= NaN`.
    assert_eq!(
        list("resource.data.n != float('NaN')", unary(FilterOp::IsNotNan)),
        Decision::Allow
    );
    assert_eq!(
        list(
            "resource.data.n != float('NaN')",
            n(FilterOp::Ne, Value::Int(1))
        ),
        Decision::Allow
    );
    assert_eq!(
        list(
            "resource.data.n > float('NaN')",
            n(FilterOp::Gt, Value::Int(0))
        ),
        denied(UNDECIDED)
    );
    // `EQ NaN` lets nothing through: it implies any comparison with a number.
    let nan_and = Filter::And(vec![
        unary(FilterOp::IsNan),
        n(FilterOp::Ne, Value::String("a".into())),
    ]);
    for allowed in [
        "resource.data.n == 1",
        "resource.data.n < 0",
        "resource.data.n == float('NaN')",
    ] {
        assert_eq!(list(allowed, nan_and.clone()), Decision::Allow, "{allowed}");
    }
    assert_eq!(list("resource.data.n == 'b'", nan_and), denied(UNDECIDED));
    // `array-contains` null or NaN is `LIST_CONTAINS []`.
    let contains_null = field("tags", FilterOp::ArrayContains, Value::Null);
    assert_eq!(
        list("[] in resource.data.tags", contains_null.clone()),
        Decision::Allow
    );
    assert_eq!(
        list("null in resource.data.tags", contains_null),
        denied(UNDECIDED)
    );
}

#[test]
fn filters_that_repeat_count_once() {
    let n = |op, v| field("n", op, v);
    let twice = |filter: Filter| Filter::And(vec![filter.clone(), filter]);
    // One `EQ` left: the value.
    assert_eq!(
        list(
            "resource.data.n is int",
            twice(n(FilterOp::Eq, Value::Int(1)))
        ),
        Decision::Allow
    );
    assert_eq!(
        list(
            "resource.data.n == null",
            twice(field("n", FilterOp::IsNull, Value::Null))
        ),
        Decision::Allow
    );
    // `1` and `1.0` are two filters; a float in an `EQ` is not equal to an int.
    let one_and_one_point_zero = Filter::And(vec![
        n(FilterOp::Eq, Value::Int(1)),
        n(FilterOp::Eq, Value::Float(1.0)),
    ]);
    assert_eq!(
        list(
            "resource.data.n is constraint",
            one_and_one_point_zero.clone()
        ),
        Decision::Allow
    );
    assert_eq!(
        list("resource.data.n != 1", one_and_one_point_zero),
        Decision::Allow
    );
}

#[test]
fn comparisons_with_true_false_and_values_without_an_order() {
    let true_and_below_five = Filter::And(vec![
        field("n", FilterOp::Eq, Value::Bool(true)),
        field("n", FilterOp::Lt, Value::Int(5)),
    ]);
    assert_eq!(
        list("resource.data.n != false", true_and_below_five.clone()),
        Decision::Allow
    );
    assert_eq!(
        list("resource.data.n != true", true_and_below_five.clone()),
        denied(UNDECIDED)
    );
    for (condition, received) in [
        ("resource.data.n >= true", "bool"),
        ("null < resource.data.n", "null"),
        ("resource.data.n <= [1]", "list"),
        ("resource.data.n > latlng.value(0, 0)", "latlng"),
    ] {
        assert_eq!(
            list(condition, true_and_below_five.clone()),
            denied(&format!(
                "\nType error. Received: [{received}] Expected: \
                 [timestamp,duration,float,int,string,bytes]. for 'list' @ L5"
            )),
            "{condition}"
        );
    }
}

#[test]
fn array_membership() {
    let contains = || field("tags", FilterOp::ArrayContains, Value::String("a".into()));
    assert_eq!(
        list("'a' in resource.data.tags", contains()),
        Decision::Allow
    );
    assert_eq!(
        list("resource.data.tags.hasAny(['a', 'b'])", contains()),
        Decision::Allow
    );
    assert_eq!(
        list("resource.data.tags.hasAny(['b'])", contains()),
        denied("\nfalse for 'list' @ L5")
    );
    assert_eq!(
        list("'b' in resource.data.tags", contains()),
        denied(UNDECIDED)
    );
    assert_eq!(
        list("resource.data.tags.hasAll(['a'])", contains()),
        denied("\nFunction not found error: Name: [hasAll]. for 'list' @ L5")
    );
    let any = field(
        "tags",
        FilterOp::ArrayContainsAny,
        Value::List(vec![Value::String("a".into()), Value::String("b".into())]),
    );
    assert_eq!(list("'a' in resource.data.tags", any), denied(UNDECIDED));
}

#[test]
fn every_way_of_the_filter_must_be_allowed() {
    let one_or_two = || {
        field(
            "n",
            FilterOp::In,
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        )
    };
    assert_eq!(list("resource.data.n > 0", one_or_two()), Decision::Allow);
    assert_eq!(
        list("resource.data.n == 1", one_or_two()),
        denied("\nfalse for 'list' @ L5")
    );
    let either = Filter::Or(vec![
        field("n", FilterOp::Eq, Value::Int(1)),
        field("s", FilterOp::Eq, Value::String("x".into())),
    ]);
    assert_eq!(
        list(
            "resource.data.n == 1 || resource.data.s == 'x'",
            either.clone()
        ),
        Decision::Allow
    );
    assert_eq!(
        list("resource.data.n == 1", either),
        denied("\nProperty n is undefined on object. for 'list' @ L5")
    );
    assert_eq!(
        list_in("/{path=**}/c/{d}", "resource.data.n == 1", None, false),
        denied("\nProperty n is undefined on object. for 'list' @ L5")
    );
}

#[test]
fn a_named_document_is_decided_as_stored() {
    let name = |d: &str| Filter::Name {
        op: FilterOp::Eq,
        value: Value::Path(document_path(d)),
    };
    assert_eq!(list("resource.data.n == 1", name("c/d1")), Decision::Allow);
    assert_eq!(list("resource.id == 'd1'", name("c/d1")), Decision::Allow);
    assert_eq!(list("resource == null", name("c/missing")), Decision::Allow);
    let named_and_filtered =
        Filter::And(vec![name("c/d1"), field("n", FilterOp::Eq, Value::Int(2))]);
    assert_eq!(
        list("resource.data.n == 1", named_and_filtered),
        Decision::Allow
    );
}

#[test]
fn paths_and_wildcards() {
    let one = || Some(field("n", FilterOp::Eq, Value::Int(1)));
    assert_eq!(
        list_in("/c/{d}", "d == 'x'", one(), false),
        denied("\nNull value error. for 'list' @ L5")
    );
    assert_eq!(
        list_in(
            "/c/{d}",
            "request.path == /databases/$(database)/documents/c",
            one(),
            false
        ),
        Decision::Allow
    );
    assert_eq!(
        list_in("/{rest=**}", "rest[0] == 'c'", one(), false),
        denied("\nVariable  is not bound in path template. for 'list' @ L5")
    );
    assert_eq!(
        list_in("/c/{d}", "true", one(), true),
        denied("No matching allow statements")
    );
    assert_eq!(
        list_in("/{path=**}/c/{d}", "true", one(), true),
        Decision::Allow
    );
    assert_eq!(
        list_in("/{path=**}/c/{d}", "request.path != null", one(), true),
        denied(UNDECIDED)
    );
}
