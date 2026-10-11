#!/usr/bin/env python3
"""How does the official emulator evaluate Security Rules: operators, errors, the library? (#60, #62, #65)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`; under another locale every warning and
evaluation error comes back as a 500):
    python3 -I tools/oracle/rules_eval.py 127.0.0.1:8086 > crates/hidane-rules/tests/fixtures/rules_eval.json

Like `rules_conditions.py`, each condition is loaded as

    rules_version = '2';
    service cloud.firestore {
      match /databases/{database}/documents {
        match /rooms/{room} {
          allow read: if <condition>;
        }
      }
    }

and `rooms/r1` (the fields in `DOCUMENT`, written once as the owner, like `rooms/r2` with `n: 1`)
is read over REST without an `Authorization` header. The cases cover operators and their
errors, literals, indexes and slices, conversions, the namespaced functions, every method of
every type (each method called on a value of each type, with arguments no form takes, which
makes the error list the forms), lookups, and whole rulesets for scoping, functions and the
order of a denial's entries. Last, batches of writes to `c/{d}` show which documents cost
nothing to look up.

Each result has `outcome` (`allow`, `deny`, `compile`, or another HTTP status), the full
`message` of a denial or a compile error, and the compiler's warnings (`issues`). A denial lists
one entry per statement, `<false or the error> for 'get' @ L<line>`, joined by `, `; after an
error the official emulator evaluates again and lists them all twice, and a condition that reads
`resource` or a document starts with `evaluation error at L5:22` (firebase-tools #6252).
Stdlib only.
"""

import http.client
import json
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
PROJECT = "rules-eval"
DB = f"projects/{PROJECT}/databases/(default)"
DOC = f"/v1/{DB}/documents/rooms/r1"

# The document of rules_conditions.py.
DOCUMENT = {
    "n": {"integerValue": "1"},
    "f": {"doubleValue": 1.5},
    "one_f": {"doubleValue": 1.0},
    "s": {"stringValue": "abc"},
    "b": {"booleanValue": True},
    "nul": {"nullValue": None},
    "arr": {"arrayValue": {"values": [{"integerValue": "1"}, {"integerValue": "2"}, {"integerValue": "3"}]}},
    "tags": {"arrayValue": {"values": [{"stringValue": "a"}, {"stringValue": "b"}]}},
    "m": {"mapValue": {"fields": {"x": {"integerValue": "1"}, "y": {"stringValue": "z"}}}},
    "ts": {"timestampValue": "2020-01-02T03:04:05.123Z"},
    "geo": {"geoPointValue": {"latitude": 10.0, "longitude": 20.0}},
    "ref": {"referenceValue": f"{DB}/documents/rooms/r2"},
    "by": {"bytesValue": "AQI="},
    "big": {"integerValue": "9223372036854775807"},
}

RULES = """rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /rooms/{room} {
      allow read: if %s;
    }
  }
}
"""

CONDITIONS = {
    "logic": [
        "1 && false", "false && 1", "true && 1", "1 || true", "true || 1", "false || 1", "1 || false",
        "'a' && true", "1", "'abc'", "null", "1 ? true : false", "null ? true : false", "!1", "!null",
    ],
    "access": [
        "'abc'.x == 1", "(1).x == 1", "resource.data.n.x == 1", "resource.data['missing'] == 1",
        "resource.data['n'] == 1", "resource.data[1] == 1", "resource.data.arr[-1] == 1",
        "resource.data.arr['a'] == 1", "resource.data.arr[1.0] == 2", "'abc'[1] == 'b'", "'abc'[5] == 'b'",
        "'abc'[-1] == 'c'", "'abc'[1.5] == 'a'", "null[0] == 1", "resource.data.nul[0] == 1",
        "resource.data.nul.x == 1", "b'abc'[0] == 97", "[1].toSet()[0] == 1", "request.path.x == 1",
        "request.path[0] == 'databases'", "request.path[1.5] == 1", "request.path[null] == 1",
        "/a/b[5] == 'b'", "/a/b[-1] == 'b'", "/a/b['x'] == 'b'",
    ],
    "slices": [
        "resource.data.arr[1:2] == [2]", "resource.data.arr[1:5] == [2]", "resource.data.arr[2:1] == []",
        "resource.data.arr[-1:2] == [1, 2]", "resource.data.arr[1:1] == []", "resource.data.arr[0:3] == [1, 2, 3]",
        "resource.data.arr[3:3] == []", "resource.data.arr[5:6] == []", "resource.data.arr[5:1] == []",
        "resource.data.arr[0:-1] == []", "resource.data.arr[null:2] == [1, 2]", "resource.data.arr[1:null] == [2, 3]",
        "resource.data.arr[null:null] == [1, 2, 3]", "resource.data.arr[1:'a'] == []",
        "resource.data.arr['a':1] == []", "'abcdef'[1:3] == 'bc'", "'abcdef'[4:9] == 'ef'", "'abcdef'[3:1] == ''",
        "''[0:0] == ''", "'abc'[3:3] == ''", "'abc'[0:3] == 'abc'", "'abc'[0:0] == ''", "'abc'[1:0] == ''",
        "'abc'[2:0] == ''", "'abc'[null:0] == ''", "[][null:null] == []", "''[null:null] == ''",
        "[][0:null] == []", "null[1:2] == 1", "(1)[1:2] == 1", "resource.data.m[1:2] == 1", "b'abc'[0:1] == b'a'",
        "request.path[1:3] == 1", "request.path[0:2] == /databases/$(database)", "request.path[-1:1] == 1",
        "string(request.path[1:1]) == '/'", "string(request.path[1:2]) == '/(default)'",
        "request.path[1:2] is path", "/a/b[0:5] == /a/b", "/a/b/c[1:3] == /b/c",
    ],
    "in": [
        "1 in 'abc'", "'a' in 'abc'", "1 in 1", "1 in null", "'x' in resource.data.m", "'n' in resource.data",
        "1 in [1.0]", "1.0 in [1]", "[1] in [[1]]", "{'a': 1} in [{'a': 1}]", "1 in {'a': 1}",
        "1 in [1].toSet()", "1.0 in [1].toSet()", "'b' in /a/b", "'a' in {'a': 1}.diff({}).addedKeys()",
    ],
    "is": [
        "1 is number", "1.5 is number", "'a' is number", "1 is constraint", "request.path is path",
        "request.time is timestamp", "duration.value(1, 's') is duration", "latlng.value(0, 0) is latlng",
        "b'' is bytes", "null is map", "[] is vector", "[1].toSet() is set", "[1].toSet() is list",
        "[1].toSet() is map", "[1].toSet() is constraint", "{'a': 1}.diff({}) is map_diff",
        "{'a': 1}.diff({}) is map", "{'a': 1}.keys() is list", "{'a': 1}.diff({}).addedKeys() is set",
        "resource.data.one_f is float", "resource.data.big is int", "resource.data.ref is path",
        "resource.data.geo is latlng", "resource.data.ts is timestamp", "resource.data.by is bytes",
        "resource.data.nul is map", "resource.data.missing is string",
    ],
    "equality": [
        "null == null", "1 == 1.0", "1 == true", "'1' == 1", "null == false", "[1, 2] == [1, 2]",
        "[1] == [1.0]", "{'a': 1} == {'a': 1.0}", "{'a': 1, 'b': 2} == {'b': 2, 'a': 1}", "[1] != [1.0]",
        "1.0 / 0.0 == 1.0 / 0.0", "0.0 == -0.0", "(0.0 / 0.0) == (0.0 / 0.0)", "request.time == 1",
        "request.time == null", "duration.value(0, 's') == null", "latlng.value(0, 0) == null",
        "request.path == null", "[1].toSet() == null", "{'a': 1}.diff({}) == null", "/a/b == '/a/b'",
        "[1].toSet() == [1]", "[1].toSet() == [1.0].toSet()", "[[1]].toSet() == [[1.0]].toSet()",
        "[{'a': 1}].toSet() == [{'a': 1.0}].toSet()", "[3, 1, 2].toSet() == [1, 2, 3].toSet()",
        "[0.0].toSet() == [-0.0].toSet()", "{'a': 1}.diff({'a': 1}) == {'a': 1}.diff({'a': 1})",
        "resource.data.geo == latlng.value(10, 20)", "resource.data.ref == /databases/$(database)/documents/rooms/r2",
        "resource.data.ref == path('databases/(default)/documents/rooms/r2')", "resource.data.by == b'\\x01\\x02'",
        "duration.value(1, 's') == duration.value(1000, 'ms')", "resource.data.ts == resource.data.ts",
    ],
    "comparison": [
        "null < 1", "1 < null", "1 < 1.5", "1.5 > 1", "'a' < 'b'", "'B' < 'a'", "'é' > 'z'", "b'a' < b'b'",
        "b'\\x80' > b'\\x7f'", "true > false", "[1] < [2]", "{'a': 1} < {'a': 1}", "request.time < request.time",
        "resource.data.ts < request.time", "duration.value(1, 's') < duration.value(2, 's')",
        "duration.value(1, 's') < 1", "resource.data.geo < latlng.value(10.0, 20.0)",
        "resource.data.ref < resource.data.ref", "(0.0 / 0.0) < 1.0", "(0.0 / 0.0) >= 1.0", "'a' <= 1",
        "'a' >= 1.0",
    ],
    "arithmetic": [
        "null + 1 == 1", "1 + null == 1", "-null == 1", "1 + 1.5 == 2.5", "3 / 2 == 1", "-3 / 2 == -1",
        "-3 % 2 == -1", "3 % -2 == 1", "3.5 % 2 == 1.5", "-3.5 % 2 == -1.5", "5 % 2.5 == 0.0", "3 / 2.0 == 1.5",
        "1 / 0.0 > 100", "1 % 0 == 0", "1.0 % 0.0 == 1", "1 % 0.0 == 1", "1 / 0 == 0", "1.0 / 0 > 100",
        "9223372036854775807 + 1 == 0", "9223372036854775807 * 2 == 0", "-9223372036854775807 - 2 == 0",
        "-(-9223372036854775807 - 1) == 0", "(-9223372036854775807 - 1) / -1 == 0",
        "(-9223372036854775807 - 1) % -1 == 0", "-9223372036854775808 == -9223372036854775807 - 1",
        "-9223372036854775808 is int", "'a' * 2 == 'aa'", "'a' - 'a' == ''", "'a' / 2 == 1", "'a' % 2 == 1",
        "true + true", "-'a' == 1", "'a' + 1 == 1", "1 + 'a' == 1", "1.5 + 'a' == 1", "'a' + 'b' == 'ab'",
        "[1] + [2] == [1, 2]", "resource.data.big + 1 > 0", "resource.data.big + 1.0 > 0",
        "resource.data.big * 1.0 == 9223372036854775807.0",
        "duration.value(1, 's') + duration.value(2, 's') == duration.value(3, 's')",
        "request.time + duration.value(1, 's') > request.time", "duration.value(1, 's') + request.time > request.time",
        "request.time - duration.value(1, 's') < request.time", "request.time - request.time == duration.value(0, 's')",
        "duration.value(1, 's') - duration.value(2, 's') == duration.value(-1, 's')",
        "-duration.value(1, 's') == duration.value(-1, 's')", "duration.value(1, 's') * 2 == duration.value(2, 's')",
        "timestamp.date(9999, 12, 31) + duration.value(1, 'd') > request.time",
        "request.time - timestamp.date(1, 1, 1) > duration.value(1, 's')",
        "duration.value(315576000000, 's') + duration.value(1, 's') > duration.value(1, 's')",
        "duration.value(-315576000000, 's') - duration.value(1, 's') < duration.value(1, 's')",
        "-duration.value(-315576000000, 's') > duration.value(1, 's')",
    ],
    "literals": [
        "{'a': 1, 'a': 2}.a == 2", "{'a': 1, 'a': 2}.size() == 1", "{1: 1}.size() == 1", "{null: 1}.size() == 1",
        "{'a': 1, 1: 2}.size() == 1", "/a/$(1) == /a/1", "/a/$(1.5) == /a/1.5", "/a/$(true) == /a/true",
        "/a/$(null) == /a/null", "/a/$(/b/c) == /a/b/c", "/a/$('b/c') == /a/b/c", "/a/$('b/c')[1] == 'b/c'",
        "/a/$('') == /a", "/a/$([1]) == /a", "/a/$({'x': 1}) == /a", "/a/$(request.time) == /a",
        "/a/%20 == /a/%20", "string(/a/%20) == '/a/%20'", "string(/a/$('x y')) == '/a/x y'",
        "/a/$('x y') == /a/x%20y", "/a/b+c == /a/$('b c')", "/a/%2F == /a/$('/')", "/a/$('é')[1] == 'é'",
        "/a/b == path('a/b')", "'\\u00e9' == 'é'", "-9223372036854775809 == 1", "-(9223372036854775808) == 1",
        "- 9223372036854775808 == 1", "1 - 9223372036854775808 == 1", "[9223372036854775808] == 1",
    ],
    "conversions": [
        "int('12') == 12", "int(' 12') == 12", "int('+12') == 12", "int('-12') == -12", "int('1.5') == 1",
        "int('1e3') == 1000", "int('0x10') == 16", "int('9223372036854775808') == 0", "int(1.9) == 1",
        "int(-1.9) == -1", "int(true) == 1", "int(null) == 0", "int(b'1') == 1", "int(1, 2) == 1", "int() == 1",
        "float('1.5') == 1.5", "float(' 1.5') == 1.5", "float('1e3') == 1000.0", "float('NaN') == 0.0",
        "float('Infinity') > 0.0", "float('inf') > 0.0", "float('abc') == 1.0", "float(1) == 1.0",
        "float(true) == 1.0", "float(null) == 1.0", "string(1) == '1'", "string(1.5) == '1.5'",
        "string(1.0) == '1.0'", "string(100000000.0) == '1.0E8'", "string(10000000.0) == '1.0E7'",
        "string(1000000.0) == '1000000.0'", "string(0.001) == '0.001'", "string(0.0001) == '1.0E-4'",
        "string(-0.0) == '-0.0'", "string(null) == 'null'", "string(true) == 'true'", "string(/a/b) == '/a/b'",
        "string(request.path) == '/databases/(default)/documents/rooms/r1'", "string([1]) == '[1]'",
        "string({'a': 1}) == ''", "string(request.time) == ''", "string(duration.value(1, 's')) == ''",
        "string(latlng.value(1, 2)) == ''", "string(b'a') == 'a'", "path('a/b') == /a/b", "path('/a/b') == /a/b",
        "path('/a//b') == /a/b", "path('a/b/') == /a/b", "path('') == /a/b", "path('/') == /a/b",
        "path(1) == /a", "path(/a/b) == /a/b", "path(null) == /a", "bool('true')", "debug(1) == 1",
        "debug(resource.data.n) == 1", "size('abc') == 3", "f_unknown(1) == 1",
    ],
    "math": [
        "math.abs(-1) == 1", "math.abs(-1.5) == 1.5", "math.abs(-9223372036854775807 - 1) == 0", "math.abs('a') == 1",
        "math.abs(null) == 1", "math.abs(1, 2) == 1", "math.abs(-0.0) == 0", "math.ceil(1.2) == 2",
        "math.ceil(1.2) is int", "math.ceil(1.2) is float", "math.ceil(1) == 1", "math.ceil(1) is int",
        "math.ceil(-0.5) == 0", "math.ceil('a') == 1", "math.floor(-1.2) == -2", "math.floor(-1.2) is int",
        "math.floor(-0.5) == 0", "math.floor('a') == 1", "math.round(2.5) == 3", "math.round(-2.5) == -3",
        "math.round(-2.5) == -2", "math.round(0.5) == 1", "math.round(1.5) is int", "math.round(3) == 3",
        "math.round(-0.5) == 0", "math.round(0.49999999999999994) == 0", "math.round('a') == 1",
        "math.sqrt(4) == 2.0", "math.sqrt(4) is float", "math.sqrt(-1) == 1", "math.sqrt('a') == 1",
        "math.isNaN(0.0 / 0.0)", "math.isNaN(1)", "math.isNaN('a')", "math.isInfinite(1.0 / 0.0)",
        "math.pow(2, 3) == 8.0", "math.pow(2, 3) is float", "math.pow(2.0, 0.5) > 1.4", "math.pow('a', 1) == 1",
        "math.foo(1) == 1",
    ],
    "time": [
        "timestamp.value(1000).toMillis() == 1000", "timestamp.value(-1000).toMillis() == -1000",
        "timestamp.value(253402300800000) == timestamp.value(0)", "timestamp.value(253402300799999).year() == 9999",
        "timestamp.value(-62135596800000).year() == 1", "timestamp.value(-62135596800001).year() == 1",
        "timestamp.value(9223372036854775807) == 1", "timestamp.value('a') == 1", "timestamp.value(null) == 1",
        "timestamp.value(1.5) == 1", "timestamp.value('2020-01-02T03:04:05Z') == timestamp.value(1577934245000)",
        "timestamp.value('2020-01-02T03:04:05.123Z') == resource.data.ts",
        "timestamp.value('2020-01-02T03:04:05.123456789Z').nanos() == 123456789",
        "timestamp.value('2020-01-02T03:04:05+01:00') == timestamp.value('2020-01-02T02:04:05Z')",
        "timestamp.value('2020-01-02') == timestamp.date(2020, 1, 2)", "timestamp.value('2020-01-02T03:04:05') == 1",
        "timestamp.value('2020-01-02t03:04:05z') == timestamp.value('2020-01-02T03:04:05Z')",
        "timestamp.value('2020-01-02 03:04:05Z') == timestamp.value('2020-01-02T03:04:05Z')",
        "timestamp.value('2020-13-02T03:04:05Z') == timestamp.value('2021-01-02T03:04:05Z')",
        "timestamp.value('2020-01-02T03:04:05.1234567891Z') == timestamp.value('2020-01-02T03:04:05.123456789Z')",
        "timestamp.value('2020-02-30T03:04:05Z') == timestamp.value('2020-03-01T03:04:05Z')",
        "timestamp.value('2020-01-02T25:04:05Z') == timestamp.value('2020-01-03T01:04:05Z')",
        "timestamp.value('2020-01-02T03:04:05.Z') == timestamp.value('2020-01-02T03:04:05Z')",
        "timestamp.value('2020-01-02T03:04:05.1Z').nanos() == 100000000",
        "timestamp.value('2020-1-2T3:4:5Z') == timestamp.value('2020-01-02T03:04:05Z')",
        "timestamp.value('2020-01-02T03:04:05+0100') == timestamp.value('2020-01-02T02:04:05Z')",
        "timestamp.date(2020, 1, 2).toMillis() == 1577923200000", "timestamp.date(2020, 2, 29).day() == 29",
        "timestamp.date(2019, 2, 29).day() == 29", "timestamp.date(2020, 2, 30).toMillis() == 0",
        "timestamp.date(2020, 13, 1).toMillis() == 0", "timestamp.date(2020, 0, 1).toMillis() == 0",
        "timestamp.date(0, 1, 1).toMillis() == 0", "timestamp.date(10000, 1, 1).toMillis() == 0",
        "timestamp.date(2020, 1, 'a').toMillis() == 0", "resource.data.ts.year() == 2020",
        "resource.data.ts.month() == 1", "resource.data.ts.day() == 2", "resource.data.ts.hours() == 3",
        "resource.data.ts.minutes() == 4", "resource.data.ts.seconds() == 5", "resource.data.ts.nanos() == 123000000",
        "resource.data.ts.toMillis() == 1577934245123", "resource.data.ts.dayOfWeek() == 4",
        "resource.data.ts.dayOfYear() == 2", "resource.data.ts.date() == timestamp.date(2020, 1, 2)",
        "resource.data.ts.time() == duration.time(3, 4, 5, 123000000)",
        "timestamp.date(2020, 1, 5).dayOfWeek() == 7", "timestamp.date(2020, 1, 6).dayOfWeek() == 1",
        "timestamp.date(2020, 12, 31).dayOfYear() == 366", "timestamp.value(-1).toMillis() == -1",
        "timestamp.value(-1).nanos() == 999000000", "timestamp.value(-1).seconds() == 59",
        "timestamp.value(-1).year() == 1969", "request.time > timestamp.date(2026, 1, 1)",
        "request.time.nanos() % 1000000 == 0",
        "duration.value(1, 'w') == duration.value(7, 'd')", "duration.value(1, 'h') == duration.value(60, 'm')",
        "duration.value(1, 'ns').nanos() == 1", "duration.value(1, 'us') == duration.value(1000, 'ns')",
        "duration.value(1, 'x') == duration.value(1, 's')", "duration.value(1, 'S') == duration.value(1, 's')",
        "duration.value(1.5, 's') == duration.value(1, 's')", "duration.value(1, 1) == duration.value(1, 's')",
        "duration.value(9223372036854775807, 'w') == duration.value(1, 's')",
        "duration.value(315576000001, 's') == duration.value(1, 's')",
        "duration.value(315576000000, 's').seconds() == 315576000000",
        "duration.value(1, 'ms').nanos() == 1000000", "duration.value(1, 'ms').seconds() == 0",
        "duration.value(-1500, 'ms').seconds() == -1", "duration.value(-1500, 'ms').nanos() == -500000000",
        "duration.value(-1, 's').nanos() == 0", "duration.time(1, 2, 3, 4).seconds() == 3723",
        "duration.time(1, 2, 3, 4).nanos() == 4", "duration.time(1, 2, 3, 1000000000).seconds() == 3724",
        "duration.time(-1, 0, 0, 0).seconds() == -3600", "duration.time(0, 0, 0, -1).nanos() == -1",
        "duration.time(0, 0, 1, -1).nanos() == 999999999", "duration.time(0, 0, 1, -1).seconds() == 0",
        "duration.abs(duration.value(-1, 's')) == duration.value(1, 's')", "duration.abs(1) == 1",
        "duration.abs(duration.value(-1500, 'ms')).nanos() == 500000000",
        "duration.abs(duration.value(-315576000000, 's')) > duration.value(1, 's')",
    ],
    "latlng": [
        "latlng.value(10, 20).latitude() == 10.0", "latlng.value(91, 20).latitude() == 91.0",
        "latlng.value(10, 181).longitude() == 181.0", "latlng.value(-90, -180).longitude() == -180.0",
        "latlng.value('a', 1) == 1", "latlng.value(0, 0).distance(latlng.value(0, 1)) > 111195.0837",
        "latlng.value(0, 0).distance(latlng.value(0, 1)) < 111195.0838",
        "latlng.value(10, 20).distance(latlng.value(-30, 100)) > 9616929.3977",
        "latlng.value(10, 20).distance(latlng.value(-30, 100)) < 9616929.3978",
        "latlng.value(0, 0).distance(1) == 1", "latlng.value(0, 0).distance(null) == 1",
        "resource.data.geo.latitude() == 10.0", "resource.data.geo.longitude() == 20.0",
        "resource.data.geo.distance(resource.data.geo) == 0.0",
    ],
    "hashing": [
        "hashing.sha256('abc') == b'\\xba\\x78\\x16\\xbf\\x8f\\x01\\xcf\\xea\\x41\\x41\\x40\\xde\\x5d\\xae\\x22\\x23"
        "\\xb0\\x03\\x61\\xa3\\x96\\x17\\x7a\\x9c\\xb4\\x10\\xff\\x61\\xf2\\x00\\x15\\xad'",
        "hashing.sha256(b'abc') == hashing.sha256('abc')", "hashing.md5('abc').toHexString() == '900150983CD24FB0D6963F7D28E17F72'",
        "hashing.md5(b'abc').size() == 16", "hashing.crc32('abc') == b'\\xc2\\x41\\x24\\x35'",
        "hashing.crc32c('abc') == b'\\xb7\\x3f\\x4b\\x36'", "hashing.crc32('') == b'\\x00\\x00\\x00\\x00'",
        "hashing.crc32('abc') is bytes", "hashing.sha256(1) == b''", "hashing.sha256(null) == b''",
    ],
    "strings": [
        "'😀'.size() == 2", "'é'.size() == 1", "'😀a'[2] == 'a'", "'😀a'[1] == 'a'", "'😀a'[2:3] == 'a'",
        "'abc'.matches('b')", "'abc'.matches('.*b.*')", "'abc'.matches(resource.data.s + '[')",
        "'ABC'.matches('(?i)abc')", "'1a'.matches('\\\\d\\\\w')", "'é'.matches('\\\\w')", "'٣'.matches('\\\\d')",
        "'é'.matches('.')", "'a b'.matches('a\\\\sb')", "'a]'.matches('[]a]+')", "'a&'.matches('[a&]+')",
        "'a,b,,c'.split(',') == ['a', 'b', '', 'c']", "'a,b,'.split(',') == ['a', 'b']", "''.split(',') == ['']",
        "',a'.split(',') == ['', 'a']", "'abc'.split('') == ['', 'a', 'b', 'c']", "'a.b'.split('.') == ['']",
        "'a1b2c'.split('[0-9]') == ['a', 'b', 'c']", "'a,,'.split(',') == ['a']", "'abc'.split('x') == ['abc']",
        "','.split(',') == ['']", "'a'.split('') == ['', 'a']", "',a,'.split(',') == ['', 'a']",
        "'ab'.split('b*') == ['', 'a']", "'a/b/c'.split('/') == ['a', 'b', 'c']",
        "'abc'.replace('(b)', '[$1]') == 'a[b]c'", "'abc'.replace('b', '\\\\$') == 'a$c'",
        "'aaa'.replace('a', 'b') == 'bbb'", "'abc'.replace('B', 'x') == 'abc'", "'abc'.replace('(b)', '$11') == 'ab1c'",
        "'abc'.replace('(?P<x>b)', '${x}') == 'abc'", "'abc'.replace('b', '\\\\x') == 'axc'",
        "'abc'.replace('[', '') == ''", "'banana'.replace('ana', 'ee') == 'beena'", "'ABC'.lower() == 'abc'",
        "'abc'.replace('b', '$') == 'a$c'", "'abc'.replace('b', '$2') == 'a$c'", "'abc'.replace('b', '\\\\') == 'a$c'",
        "'abc'.upper() == 'ABC'", "'ß'.upper() == 'ß'", "'ÉA'.lower() == 'Éa'", "'éa'.upper() == 'éA'",
        "'ΣΑΣ'.lower() == 'ΣΑΣ'", "'  a b  '.trim() == 'a b'", "'\\ta\\n'.trim() == 'a'",
        "'\\u00a0a\\u00a0'.trim() == 'a'", "'abc'.toUtf8() == b'abc'", "'é'.toUtf8().size() == 2",
    ],
    "bytes": [
        "b'\\xfb\\xff'.toBase64() == '-_8='", "b'\\xFB\\xEF\\xBE'.toBase64() == '----'", "b'a'.toBase64() == 'YQ=='",
        "b'\\x0a\\xff'.toHexString() == '0AFF'", "b'abc'.size() == 3", "resource.data.by.size() == 2",
    ],
    "lists": [
        "[1, 2].concat([3]) == [1, 2, 3]", "['a', 'b'].join(',') == 'a,b'", "[1, 2].join(',') == '1,2'",
        "[].join(',') == ''", "[1.5, true, null].join(',') == '1.5,true,null'", "[[1]].join(',') == '[1]'",
        "[b'a'].join(',') == 'a'", "[1.0].join(',') == '1.0'", "[1, 1, 2].toSet() == [1, 2].toSet()",
        "[1, 1.0].toSet().size() == 1", "[1, 2, 3, 2].removeAll([2]) == [1, 3]", "[1, 2].removeAll([1.0]) == [2]",
        "[1, 2].hasAll([1]) && [1, 2].hasAny([3, 1]) && [1, 2].hasOnly([1, 2, 3])", "[1, 2].hasAll([1.0])",
        "[].hasAll([])", "[].hasAny([])", "['a', 'a', 'b'].hasOnly(['a', 'b', 'b'])", "[1, 2].hasAll([1].toSet())",
        "[1, 2].removeAll([1].toSet()) == [2]", "[1, 2].concat([1].toSet()) == [2]", "[1, 2].size() == 2",
    ],
    "maps": [
        "{'b': 1, 'a': 2}.keys() == ['a', 'b']", "{'b': 2, 'a': 1}.values() == [2, 1]",
        "{'b': 1, 'a': 2, 'c': 0}.values() == [1, 2, 0]", "{'B': 1, 'a': 2, '_': 3}.keys() == ['B', '_', 'a']",
        "{'é': 1, 'z': 2}.keys() == ['z', 'é']", "resource.data.keys()[0] == 'arr'",
        "resource.keys() == ['__name__', 'data', 'id']", "resource.size() == 3", "resource.data.size() == 14",
        "{'a': {'b': 1}}.get(['a', 'b'], 0) == 1", "{'a': {'b': 1}}.get(['a', 'c'], 0) == 0",
        "{'a': 1}.get(['a', 'c'], 0) == 0", "{'a': 1}.get('b', 0) == 0", "{'a': 1}.get('a', 0) == 1",
        "{'a': 1}.get([], 0) == {'a': 1}", "{'a': 1}.get([1], 0) == 0",
        "{'a': 0, 'c': 0, 'u': 0}.diff({'r': 0, 'c': 1, 'u': 0}).affectedKeys() == ['a', 'r', 'c'].toSet()",
        "{'a': 1, 'b': 2}.diff({'a': 1, 'b': 3, 'c': 4}).addedKeys() == [].toSet()",
        "{'a': 1, 'b': 2, 'd': 5}.diff({'a': 1, 'b': 3, 'c': 4}).addedKeys() == ['d'].toSet()",
        "{'a': 1, 'b': 2}.diff({'a': 1, 'b': 3, 'c': 4}).removedKeys() == ['c'].toSet()",
        "{'a': 1, 'b': 2}.diff({'a': 1, 'b': 3, 'c': 4}).changedKeys() == ['b'].toSet()",
        "{'a': 1, 'b': 2}.diff({'a': 1, 'b': 3, 'c': 4}).unchangedKeys() == ['a'].toSet()",
        "{'a': 1}.diff({'a': 1.0}).changedKeys().size() == 0", "{'a': [1]}.diff({'a': [1.0]}).changedKeys().size() == 0",
        "{'a': {'b': 1}}.diff({'a': {'b': 1}}).unchangedKeys() == ['a'].toSet()",
    ],
    "sets": [
        "[1, 2].toSet().union([3].toSet()) == [1, 2, 3].toSet()", "[1, 2].toSet().intersection([2, 3].toSet()) == [2].toSet()",
        "[1, 2].toSet().difference([2, 3].toSet()) == [1].toSet()", "[1, 2].toSet().difference([1.0].toSet()) == [2].toSet()",
        "[1, 2].toSet().union([3]) == [1].toSet()", "[1].toSet().union([1].toSet()).size() == 1",
        "[1, 2].toSet().hasAll([1])", "[1].toSet().hasAll([1.0])", "['a', 'b'].toSet().hasOnly(['a', 'b'])",
        "['a', 'b'].toSet().hasAny(['c', 'd'].toSet())", "[1, 2].toSet().size() == 2",
    ],
    "paths": [
        "(/a/b).bind({'x': 1}) == /a/b", "request.path.bind({}) == request.path", "resource.data.ref[3] == 'rooms'",
        "resource.data.ref[0] == 'databases'", "resource.data.ref.size() == 5",
        "string(resource.data.ref) == '/databases/(default)/documents/rooms/r2'",
        "/databases/$(database)/documents/rooms/r1 == request.path",
    ],
    "request": [
        "request.auth == null", "request.auth.uid == null", "request.query == null", "request.method == 'get'",
        "request.resource == null", "resource.__name__ == request.path", "resource.id == 'r1'",
        "request.keys().hasAll(['auth', 'method', 'path', 'time'])", "database == '(default)'", "room == 'r1'",
        "foo == 1", "foo == 1 || true",
    ],
    "lookups": [
        "exists(/databases/$(database)/documents/rooms/r1)", "exists(/databases/$(database)/documents/rooms/missing)",
        "exists(/databases/$(database)/documents/rooms)", "exists(/databases/$(database)/documents)",
        "exists('/databases/(default)/documents/rooms/r1')", "exists(/databases/other/documents/rooms/r1)",
        "exists(/rooms/r1)", "exists(/databases/$(database)/x/rooms/r1)", "exists(null)", "exists(1)", "exists() == 1",
        "get(/databases/$(database)/documents/rooms/r1).data.n == 1",
        "get(/databases/$(database)/documents/rooms/r1).id == 'r1'",
        "get(/databases/$(database)/documents/rooms/r1).__name__ == request.path",
        "get(/databases/$(database)/documents/rooms/r1) == resource",
        "get(/databases/$(database)/documents/rooms/missing) == null",
        "get(/databases/$(database)/documents/rooms/missing).data.n == 1",
        "get(/databases/$(database)/documents/rooms/missing).data.n == 1 || true",
        "get(/databases/$(database)/documents/rooms) == null", "get(/rooms/r1) == null",
        "getAfter(/databases/$(database)/documents/rooms/r1).data.n == 1",
        "existsAfter(/databases/$(database)/documents/rooms/r1)",
        "get(/databases/$(database)/documents/rooms/r1/sub/x) == null",
        "exists(/databases/$(database)/documents/rooms/r1/sub)", "get(request.path).id == 'r1'",
        "get(resource.data.ref) == null", "exists(/databases/(default)/documents/rooms/$('r1'))",
        "value(/databases/$(database)/documents/rooms/r1) == resource",
        "value(/databases/$(database)/documents/rooms/r1).data.n == 1", "value(1) == 1",
        " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(10)),
        " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(11)),
        " && ".join("!exists(/databases/$(database)/documents/missing/m0)" for i in range(15)),
        # The document read is free; `rooms/r2` is another one.
        "get(/databases/$(database)/documents/rooms/r1) != null && "
        + " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(10)),
        "get(/databases/$(database)/documents/rooms/r2) != null && "
        + " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(10)),
        "get(/databases/$(database)/documents/rooms/r2) != null && "
        + " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(9)),
        "get(/databases/$(database)/documents/rooms/r2) != null && "
        "getAfter(/databases/$(database)/documents/rooms/r2) != null && "
        + " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(9)),
    ],
    "calls": [
        "'abc'.foo({1: 1}) == 1", "(1).size({1: 1}) == 1", "'abc'.size({1: 1}) == 1", "math.foo({1: 1}) == 1",
        "math.abs({1: 1}) == 1", "int({1: 1}, 2) == 1", "null.size({1: 1}) == 1",
        "resource.data.missing.size() == 1", "{1: 1}.foo() == 1",
    ],
}

# Each method called on a value of each type, with arguments no form takes.
VALUES = {
    "string": "'abc'", "bytes": "b'abc'", "int": "(1)", "float": "(1.5)", "bool": "true", "null": "null",
    "list": "[1]", "map": "{'a': 1}", "set": "[1].toSet()", "map_diff": "{'a': 1}.diff({'a': 2})",
    "timestamp": "request.time", "duration": "duration.value(1, 's')", "latlng": "latlng.value(1, 1)",
    "path": "request.path",
}
METHODS = [("get", 2), ("size", 0), ("matches", 1), ("keys", 0), ("hasAll", 1), ("toSet", 0), ("date", 0),
           ("time", 0), ("lower", 0), ("upper", 0), ("split", 1), ("replace", 2), ("trim", 0), ("join", 1),
           ("concat", 1), ("toMillis", 0), ("year", 0), ("month", 0), ("day", 0), ("hours", 0), ("minutes", 0),
           ("seconds", 0), ("nanos", 0), ("dayOfWeek", 0), ("dayOfYear", 0), ("distance", 1), ("latitude", 0),
           ("longitude", 0), ("bind", 1), ("toBase64", 0), ("toHexString", 0), ("toUtf8", 0), ("diff", 1),
           ("addedKeys", 0), ("affectedKeys", 0), ("changedKeys", 0), ("removedKeys", 0), ("unchangedKeys", 0),
           ("values", 0), ("difference", 1), ("intersection", 1), ("union", 1), ("removeAll", 1), ("hasAny", 1),
           ("hasOnly", 1)]
CONDITIONS["methods"] = [
    f"{v}.{m}({', '.join(['latlng.value(1, 1)'] * max(n, 1))}) == 1"
    for t, v in VALUES.items() if t != "null" for m, n in METHODS
] + [f"null.{m}() == 1" for m in ("size", "keys", "lower")]


def ruleset(body, top="", service=""):
    """`body` goes in `match /databases/{database}/documents`, `top` before the service and
    `service` at its start."""
    return ("rules_version = '2';\n" + top + "service cloud.firestore {\n" + service
            + "  match /databases/{database}/documents {\n"
            + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")


# Whole rulesets, each read with a get of rooms/r1.
RULESETS = [
    ("function at the top sees no wildcard",
     ruleset("match /rooms/{room} {\n  allow read: if f();\n}", "function f() { return room == 'r1'; }\n")),
    ("function at the top sees no database",
     ruleset("match /rooms/{room} {\n  allow read: if f();\n}", "function f() { return database == '(default)'; }\n")),
    ("function of the service sees no database",
     ruleset("match /rooms/{room} {\n  allow read: if f();\n}",
             service="  function f() { return database == '(default)'; }\n")),
    ("function of the service reads documents",
     ruleset("match /rooms/{room} {\n  allow read: if f(database);\n}",
             service="  function f(db) { return exists(/databases/$(db)/documents/rooms/r1); }\n")),
    ("function of an outer match sees no inner wildcard",
     ruleset("function f() { return room == 'r1'; }\nmatch /rooms/{room} {\n  allow read: if f();\n}")),
    ("function of an outer match sees its wildcard",
     ruleset("function f() { return database == '(default)'; }\nmatch /rooms/{room} {\n  allow read: if f();\n}")),
    ("function of a match sees its wildcard",
     ruleset("match /rooms/{room} {\n  function f() { return room == 'r1'; }\n  allow read: if f();\n}")),
    ("parameter hides a wildcard",
     ruleset("match /rooms/{room} {\n  function f(room) { return room == 'x'; }\n  allow read: if f('x');\n}")),
    ("let hides request",
     ruleset("match /rooms/{room} {\n  function f() { let request = 1; return request == 1; }\n  allow read: if f();\n}")),
    ("parameter named request",
     ruleset("match /rooms/{room} {\n  function f(request) { return request == 1; }\n  allow read: if f(1);\n}")),
    ("function calls one of an inner match",
     ruleset("function g() { return h(); }\nmatch /rooms/{room} {\n  function h() { return true; }\n  allow read: if g();\n}")),
    ("unused let with an error",
     ruleset("match /rooms/{room} {\n  function f() { let a = 1 / 0; return true; }\n  allow read: if f();\n}")),
    ("used let with an error, absorbed",
     ruleset("match /rooms/{room} {\n  function f() { let a = 1 / 0; return a == 1 || true; }\n  allow read: if f();\n}")),
    ("used let with an error",
     ruleset("match /rooms/{room} {\n  function f() { let a = 1 / 0; return a == 1; }\n  allow read: if f();\n}")),
    ("let reads an earlier let",
     ruleset("match /rooms/{room} {\n  function f() { let a = 1; let b = a + 1; return b == 2; }\n  allow read: if f();\n}")),
    ("unused argument with an error",
     ruleset("match /rooms/{room} {\n  function f(x) { return true; }\n  allow read: if f(1 / 0);\n}")),
    ("too few arguments",
     ruleset("match /rooms/{room} {\n  function f(x) { return true; }\n  allow read: if f();\n}")),
    ("too many arguments",
     ruleset("match /rooms/{room} {\n  function f(x, y) { return x == y; }\n  allow read: if f(1, 2, 3);\n}")),
    ("function returns an int",
     ruleset("match /rooms/{room} {\n  function f() { return 1; }\n  allow read: if f();\n}")),
    ("function reads a document",
     ruleset("function n(id) {\n  return get(/databases/$(database)/documents/rooms/$(id)).data.n;\n}\n"
             "match /rooms/{room} {\n  allow read: if n('r1') == 1;\n}")),
    ("nested match before the allow around it",
     ruleset("match /{p=**} {\n  match /rooms/{room} {\n    allow read: if false;\n  }\n  allow read: if 1 == 2;\n}")),
    ("two matches, the outer one later",
     ruleset("match /rooms/{room} {\n  allow read: if false;\n}\nmatch /{p=**} {\n  allow read: if 1 == 2;\n}")),
    ("an error and a false",
     ruleset("match /rooms/{room} {\n  allow read: if 1 / 0 == 1;\n  allow get: if false;\n}")),
    ("a false and a missing field",
     ruleset("match /rooms/{room} {\n  allow read: if false;\n  allow get: if resource.data.missing == 1;\n}")),
    ("two statements on one line",
     ruleset("match /rooms/{room} {\n  allow read: if false; allow get: if 1 == 2;\n}")),
    ("one statement naming get and read",
     ruleset("match /rooms/{room} {\n  allow get, read: if false;\n}")),
    ("condition on the next line",
     ruleset("match /rooms/{room} {\n  allow read: if\n    false;\n}")),
    ("error in a function",
     ruleset("match /rooms/{room} {\n  allow read: if f();\n}", "function f() {\n  return int('a') == 1;\n}\n")),
    ("no statement for get",
     ruleset("match /rooms/{room} {\n  allow write: if true;\n}")),
    ("no match for the path",
     ruleset("match /other/{room} {\n  allow read: if true;\n}")),
    ("recursive wildcard is a path",
     ruleset("match /{rest=**} {\n  allow read: if rest == /rooms/r1;\n}")),
    ("recursive wildcard is not a string",
     ruleset("match /{rest=**} {\n  allow read: if rest == 'rooms/r1';\n}")),
    ("empty recursive wildcard",
     ruleset("match /rooms/{room}/{rest=**} {\n  allow read: if rest is path && rest == request.path[5:5];\n}")),
    ("recursive wildcard after a wildcard",
     ruleset("match /rooms/{rest=**} {\n  allow read: if rest is path && rest[0] == 'r1';\n}")),
    ("wildcard is a string",
     ruleset("match /rooms/{room} {\n  allow read: if room is string;\n}")),
    ("wildcard of an outer match",
     ruleset("match /{c}/{d} {\n  match /x/{y} {\n    allow read: if true;\n  }\n  allow read: if c == 'rooms' && d == 'r1';\n}")),
]


def request(method, path, body=None, headers=None):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request(method, path, None if body is None else json.dumps(body),
              {"Content-Type": "application/json", **(headers or {})})
    res = c.getresponse()
    raw = res.read().decode()
    c.close()
    try:
        return res.status, json.loads(raw) if raw.strip() else None
    except ValueError:
        return res.status, raw


def run(content):
    status, body = request("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                           {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    if status != 200:
        return {"outcome": "compile", "message": body["error"]["message"]}
    r = {}
    issues = [i["description"] for i in (body or {}).get("issues", [])]
    if issues:
        r["issues"] = issues
    status, body = request("GET", DOC)
    if status == 200:
        r["outcome"] = "allow"
    elif status == 403:
        r.update(outcome="deny", message=body["error"]["message"])
    else:
        r.update(outcome=status, message=body.get("error", {}).get("message") if isinstance(body, dict) else body)
    return r


TEN = " && ".join(f"!exists(/databases/$(database)/documents/missing/m{i})" for i in range(10))
C = "/databases/$(database)/documents/c"

# Batches committed under `allow write: if <condition>` on `c/{d}`, with `c/x` stored first: which
# documents cost nothing to look up. Each write updates (`+id`) or deletes (`-id`) a document.
BATCHES = [
    ("the document written", f"getAfter({C}/$(d)) != null && {TEN}", ["+w"]),
    ("the document written, before", f"get({C}/$(d)) != null && {TEN}", ["+x"]),
    ("another document", f"get({C}/x) != null && {TEN}", ["+w"]),
    ("a document the batch writes", f"getAfter({C}/w) != null && {TEN}", ["+v", "+w"]),
    ("a document the batch writes, before", f"get({C}/x) != null && {TEN}", ["+v", "+x"]),
    ("a document the batch deletes", f"exists({C}/x) && {TEN}", ["+v", "-x"]),
    ("a document outside the batch", f"get({C}/x) != null && {TEN}", ["+v", "+u"]),
]


def batch(condition, writes):
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               f"    match /c/{{d}} {{\n      allow read: if true;\n      allow write: if {condition};\n"
               "    }\n  }\n}\n")
    status, body = request("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                           {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200, body
    base = f"{DB}/documents"
    owner = {"Authorization": "Bearer owner"}
    for d in ("u", "v", "w"):
        request("DELETE", f"/v1/{base}/c/{d}", headers=owner)
    request("PATCH", f"/v1/{base}/c/x", {"fields": {"n": {"integerValue": "1"}}}, owner)
    status, body = request("POST", f"/v1/{base}:commit", {"writes": [
        {"delete": f"{base}/c/{w[1:]}"} if w[0] == "-"
        else {"update": {"name": f"{base}/c/{w[1:]}", "fields": {"n": {"integerValue": "2"}}}}
        for w in writes]})
    if status == 200:
        return {"outcome": "allow"}
    return {"outcome": "deny" if status == 403 else status, "message": body["error"]["message"]}


def main():
    owner = {"Authorization": "Bearer owner"}
    status, _ = request("PATCH", DOC, {"fields": DOCUMENT}, owner)
    assert status == 200, status
    status, _ = request("PATCH", DOC.replace("rooms/r1", "rooms/r2"), {"fields": {"n": {"integerValue": "1"}}}, owner)
    assert status == 200, status
    results = []
    for group, conditions in CONDITIONS.items():
        for i, condition in enumerate(conditions):
            results.append({"name": f"{group}/{i}", "condition": condition, **run(RULES % condition)})
    for name, content in RULESETS:
        results.append({"name": f"rulesets/{name}", "rules": content, **run(content)})
    for name, condition, writes in BATCHES:
        results.append({"name": f"batches/{name}", "condition": condition, "writes": writes,
                        **batch(condition, writes)})
    # One result per line, so a diff shows which cases changed.
    sys.stdout.reconfigure(encoding="utf-8")
    print("{")
    print(f' "document": {json.dumps(DOCUMENT)},')
    print(f' "rules": {json.dumps(RULES)},')
    print(' "results": [')
    print(",\n".join("  " + json.dumps(r, ensure_ascii=False) for r in results))
    print(" ]")
    print("}")


if __name__ == "__main__":
    main()
