// Command sdk_ios runs one section of steps through firebase-ios-sdk's FirebaseFirestore (the
// client SDK of Apple platforms, built here for macOS) and prints a transcript without times or
// generated IDs, so the transcripts of two emulators can be diffed. FIRESTORE_EMULATOR_HOST
// selects the emulator; start it fresh for each section.
//
// Usage: sdk_ios documents|queries|transactions|listen > transcript.json
//
// The sections follow tools/oracle/sdk_go/ and the web SDK's scripts, with the steps a client
// SDK offers: reads go to the server (`source: .server`) and the cache stays in memory, so a
// transcript holds what the emulator answered. The listen section writes through a second
// instance, as another client would, and leaves its listeners idle for 95 seconds once
// (firebase-tools#11238).

import FirebaseCore
import FirebaseFirestore
import Foundation

let sections = ["documents", "queries", "transactions", "listen"]
guard CommandLine.arguments.count == 2, sections.contains(CommandLine.arguments[1]) else {
    FileHandle.standardError.write(
        "usage: sdk_ios documents|queries|transactions|listen > transcript.json\n".data(using: .utf8)!)
    exit(2)
}
let section = CommandLine.arguments[1]
let host = ProcessInfo.processInfo.environment["FIRESTORE_EMULATOR_HOST"] ?? "127.0.0.1:8080"

/// A Firestore instance of its own app, talking to the emulator, its cache in memory and its
/// callbacks off the main thread (which the harness blocks while it waits).
func instance(_ name: String) -> Firestore {
    let options = FirebaseOptions(googleAppID: "1:1:ios:1", gcmSenderID: "1")
    options.projectID = "demo-ios-\(section)"
    options.apiKey = "demo"
    FirebaseApp.configure(name: name, options: options)
    let db = Firestore.firestore(app: FirebaseApp.app(name: name)!)
    let settings = db.settings
    settings.host = host
    settings.isSSLEnabled = false
    settings.cacheSettings = MemoryCacheSettings()
    settings.dispatchQueue = DispatchQueue(label: "sdk_ios.\(name)")
    db.settings = settings
    return db
}

let db = instance("client")

// --- values ---------------------------------------------------------------------------------

/// A double as text, so that it stays apart from integers in JSON.
func number(_ d: Double) -> Any {
    if d.isNaN { return ["double": "NaN"] }
    if d.isInfinite { return ["double": d > 0 ? "Infinity" : "-Infinity"] }
    return ["double": "\(d)"]
}

/// A Firestore value as JSON: doubles tagged, timestamps as "timestamp" (or their seconds when
/// `times` is set), the other types tagged.
func norm(_ v: Any?, times: Bool = false) -> Any {
    switch v {
    case nil, is NSNull:
        return NSNull()
    case let n as NSNumber:
        if CFGetTypeID(n) == CFBooleanGetTypeID() { return n.boolValue }
        if CFNumberIsFloatType(n) { return number(n.doubleValue) }
        return n.int64Value
    case let s as String:
        return s
    case let d as Data:
        return ["bytes": d.base64EncodedString()]
    case let t as Timestamp:
        return times ? ["timestamp": [t.seconds, Int64(t.nanoseconds)]] : "timestamp"
    case let g as GeoPoint:
        return ["geopoint": [number(g.latitude), number(g.longitude)]]
    case let r as DocumentReference:
        return ["reference": r.path]
    case let vector as VectorValue:
        return ["vector": vector.array.map(number)]
    case let a as [Any]:
        return a.map { norm($0, times: times) }
    case let m as [String: Any]:
        return m.mapValues { norm($0, times: times) }
    default:
        return ["unknown": "\(type(of: v!))"]
    }
}

let codeNames = [
    0: "OK", 1: "CANCELLED", 2: "UNKNOWN", 3: "INVALID_ARGUMENT", 4: "DEADLINE_EXCEEDED",
    5: "NOT_FOUND", 6: "ALREADY_EXISTS", 7: "PERMISSION_DENIED", 8: "RESOURCE_EXHAUSTED",
    9: "FAILED_PRECONDITION", 10: "ABORTED", 11: "OUT_OF_RANGE", 12: "UNIMPLEMENTED",
    13: "INTERNAL", 14: "UNAVAILABLE", 15: "DATA_LOSS", 16: "UNAUTHENTICATED",
]

/// An error as the code the SDK gives it; one of the harness's own as its description. The
/// messages are left out: the official emulator's print its internal keys
/// (docs/parity-exceptions.md).
func errorOf(_ error: Error) -> Any {
    let e = error as NSError
    if e.domain == FirestoreErrorDomain { return ["code": codeNames[e.code] ?? "\(e.code)"] }
    return ["client": e.localizedDescription]
}

// --- the transcript -------------------------------------------------------------------------

var out: [Any] = []

func step(_ name: String, _ body: () async throws -> Any) async {
    do {
        out.append(["name": name, "ok": try await body()])
    } catch {
        out.append(["name": name, "error": errorOf(error)])
    }
}

/// A document as the server has it: its data, or null when it does not exist.
func read(_ ref: DocumentReference, times: Bool = false) async throws -> Any {
    let snap = try await ref.getDocument(source: .server)
    return snap.exists ? norm(snap.data(), times: times) : NSNull()
}

func ids(_ query: Query) async throws -> Any {
    try await query.getDocuments(source: .server).documents.map { $0.reference.path }
}

// --- documents ------------------------------------------------------------------------------

func documents() async {
    let alice = db.document("users/alice")
    await step("set alice") {
        try await alice.setData(["name": "Alice", "age": 30, "tags": ["a", "b"], "nested": ["x": 1]])
        return try await read(alice)
    }
    await step("update dotted") {
        try await alice.updateData(["age": 31, "nested.y": 2])
        return try await read(alice)
    }
    await step("update with a FieldPath holding a dot") {
        try await alice.updateData([FieldPath(["a.b"]): 1])
        return try await read(alice)
    }
    await step("set merge") {
        try await alice.setData(["city": "Tokyo", "nested": ["z": 3]], merge: true)
        return try await read(alice)
    }
    await step("set mergeFields") {
        try await alice.setData(["city": "Osaka", "age": 99], mergeFields: ["city"])
        return try await read(alice)
    }
    await step("delete a field") {
        try await alice.updateData(["city": FieldValue.delete()])
        return try await read(alice)
    }
    await step("update a missing document") {
        try await db.document("users/nobody").updateData(["x": 1])
        return "unexpected"
    }
    await step("get a missing document") { try await read(db.document("users/nobody")) }
    await step("transforms") {
        try await alice.updateData([
            "age": FieldValue.increment(Int64(2)), "score": FieldValue.increment(1.5),
            "tags": FieldValue.arrayUnion(["c", "a"]), "nested.x": FieldValue.delete(),
            "seen": FieldValue.serverTimestamp(),
        ])
        return try await read(alice)
    }
    await step("arrayRemove") {
        try await alice.updateData(["tags": FieldValue.arrayRemove(["a"])])
        return try await read(alice)
    }
    await step("server timestamp is a timestamp") {
        let snap = try await alice.getDocument(source: .server)
        return snap.get("seen") is Timestamp
    }
    await step("a value of each type") {
        let types = db.document("users/types")
        try await types.setData([
            "int": 1, "big": Int64.max, "double": 1.5, "whole double": 2.0, "negative zero": -0.0,
            "nan": Double.nan, "infinity": Double.infinity, "minus infinity": -Double.infinity,
            "string": "x", "empty": "", "bool": true, "null": NSNull(),
            "bytes": Data([1, 2, 255]), "timestamp": Timestamp(seconds: 1_577_836_800, nanoseconds: 123_456_000),
            "geopoint": GeoPoint(latitude: 1.5, longitude: -2), "reference": db.document("users/alice"),
            "array": [1, "a", ["m": true]], "map": ["a": ["b": 1]], "empty map": [String: Any](),
            "vector": FieldValue.vector([1, 2.5]),
        ])
        return try await read(types, times: true)
    }
    await step("a batch") {
        let batch = db.batch()
        batch.setData(["n": 1], forDocument: db.document("b/one"))
        batch.setData(["n": 2], forDocument: db.document("b/two"))
        batch.updateData(["n": 3], forDocument: db.document("b/one"))
        batch.deleteDocument(db.document("b/two"))
        try await batch.commit()
        return [try await read(db.document("b/one")), try await read(db.document("b/two"))]
    }
    await step("a batch that fails writes nothing") {
        let batch = db.batch()
        batch.setData(["n": 1], forDocument: db.document("b/three"))
        batch.updateData(["n": 1], forDocument: db.document("b/missing"))
        do {
            try await batch.commit()
            return "unexpected"
        } catch {
            return ["error": errorOf(error), "three": try await read(db.document("b/three"))]
        }
    }
    await step("a generated ID") {
        let ref = db.collection("generated").document()
        try await ref.setData(["n": 1])
        return ["length": ref.documentID.count, "data": try await read(ref)]
    }
    await step("delete alice") {
        try await alice.delete()
        return try await read(alice)
    }
    await step("delete a missing document") {
        try await db.document("users/nobody").delete()
        return "ok"
    }
}

// --- queries --------------------------------------------------------------------------------

func queries() async {
    let cities = db.collection("cities")
    await step("seed") {
        let batch = db.batch()
        let rows: [(String, [String: Any])] = [
            ("SF", ["pop": 860, "state": "CA", "tags": ["coast", "tech"], "area": 121.4]),
            ("LA", ["pop": 3900, "state": "CA", "tags": ["coast"], "area": 1302.0]),
            ("TOK", ["pop": 9000, "state": NSNull(), "tags": ["tech"], "area": 2194.0]),
            ("NY", ["pop": 8300, "state": "NY", "tags": [String](), "area": 783.8]),
            ("BJ", ["pop": 21500, "area": 16411.0]),
        ]
        for (id, data) in rows { batch.setData(data, forDocument: cities.document(id)) }
        batch.setData(["name": "Golden Gate"], forDocument: db.document("cities/SF/landmarks/gg"))
        batch.setData(["name": "Hollywood"], forDocument: db.document("cities/LA/landmarks/hw"))
        batch.setData(["name": "Elsewhere"], forDocument: db.document("other/x/landmarks/e"))
        try await batch.commit()
        return "ok"
    }
    await step("all") { try await ids(cities) }
    await step("pop > 1000 by pop") { try await ids(cities.whereField("pop", isGreaterThan: 1000).order(by: "pop")) }
    await step("state == CA") { try await ids(cities.whereField("state", isEqualTo: "CA")) }
    await step("state == null") { try await ids(cities.whereField("state", isEqualTo: NSNull())) }
    await step("state != CA") { try await ids(cities.whereField("state", isNotEqualTo: "CA")) }
    await step("state in [CA, NY]") { try await ids(cities.whereField("state", in: ["CA", "NY"])) }
    await step("state not-in [CA]") { try await ids(cities.whereField("state", notIn: ["CA"])) }
    await step("tags array-contains tech") { try await ids(cities.whereField("tags", arrayContains: "tech")) }
    await step("tags array-contains-any") {
        try await ids(cities.whereField("tags", arrayContainsAny: ["coast", "none"]))
    }
    await step("or") {
        try await ids(cities.whereFilter(Filter.orFilter([
            Filter.whereField("state", isEqualTo: "NY"), Filter.whereField("pop", isLessThan: 1000),
        ])))
    }
    await step("and of an or") {
        try await ids(cities.whereFilter(Filter.andFilter([
            Filter.whereField("area", isGreaterThan: 500),
            Filter.orFilter([Filter.whereField("state", isEqualTo: "CA"), Filter.whereField("pop", isGreaterThan: 10000)]),
        ])))
    }
    await step("top 2 by pop") { try await ids(cities.order(by: "pop", descending: true).limit(to: 2)) }
    await step("last 2 by pop") { try await ids(cities.order(by: "pop").limit(toLast: 2)) }
    await step("cursors") {
        try await ids(cities.order(by: "pop").start(at: [1000]).end(before: [9000]))
    }
    await step("cursor after a document") {
        let la = try await cities.document("LA").getDocument(source: .server)
        return try await ids(cities.order(by: "pop").start(afterDocument: la))
    }
    await step("two inequalities") {
        try await ids(cities.whereField("pop", isGreaterThan: 1000).whereField("area", isLessThan: 2000))
    }
    await step("document ID in") {
        try await ids(cities.whereField(FieldPath.documentID(), in: ["SF", "NY", "XX"]))
    }
    await step("order by document ID, descending") {
        try await ids(cities.order(by: FieldPath.documentID(), descending: true))
    }
    await step("collection group") { try await ids(db.collectionGroup("landmarks")) }
    await step("collection group, filtered") {
        try await ids(db.collectionGroup("landmarks").whereField("name", isGreaterThan: "G"))
    }
    await step("count") {
        let snap = try await cities.whereField("state", isEqualTo: "CA").count.getAggregation(source: .server)
        return norm(snap.count)
    }
    await step("sum and average") {
        let snap = try await cities.aggregate([AggregateField.sum("pop"), AggregateField.average("area"),
                                               AggregateField.count()]).getAggregation(source: .server)
        return [norm(snap.get(AggregateField.sum("pop"))), norm(snap.get(AggregateField.average("area"))),
                norm(snap.get(AggregateField.count()))]
    }
    await step("average of a missing field") {
        let snap = try await cities.aggregate([AggregateField.average("nope")]).getAggregation(source: .server)
        return norm(snap.get(AggregateField.average("nope")))
    }
    await step("a query an index would serve, ordered by a missing field") {
        try await ids(cities.order(by: "missing"))
    }
}

// --- transactions ---------------------------------------------------------------------------

func transactions() async {
    let counter = db.document("t/counter")
    await step("create in a transaction") {
        let result = try await db.runTransaction { transaction, _ in
            let snap = try? transaction.getDocument(counter)
            transaction.setData(["n": 1], forDocument: counter)
            return snap?.exists ?? false
        }
        return ["existed": norm(result), "now": try await read(counter)]
    }
    await step("read and increment") {
        let result = try await db.runTransaction { transaction, errorPointer in
            do {
                let snap = try transaction.getDocument(counter)
                let n = (snap.get("n") as? Int64) ?? 0
                transaction.updateData(["n": n + 1], forDocument: counter)
                return n + 1
            } catch {
                errorPointer?.pointee = error as NSError
                return nil
            }
        }
        return ["returned": norm(result), "now": try await read(counter)]
    }
    await step("write only") {
        _ = try await db.runTransaction { transaction, _ in
            transaction.setData(["w": true], forDocument: db.document("t/write-only"))
            return nil
        }
        return try await read(db.document("t/write-only"))
    }
    await step("update a missing document") {
        _ = try await db.runTransaction { transaction, _ in
            transaction.updateData(["n": 1], forDocument: db.document("t/missing"))
            return nil
        }
        return "unexpected"
    }
    await step("a callback that fails") {
        do {
            _ = try await db.runTransaction { transaction, errorPointer in
                transaction.setData(["n": 99], forDocument: counter)
                errorPointer?.pointee = NSError(domain: "harness", code: 1,
                                                userInfo: [NSLocalizedDescriptionKey: "the callback failed"])
                return nil
            }
            return "unexpected"
        } catch {
            return ["error": errorOf(error), "now": try await read(counter)]
        }
    }
    await step("an outside write between the read and the commit") {
        var attempts = 0
        let result = try await db.runTransaction { transaction, errorPointer in
            attempts += 1
            do {
                let snap = try transaction.getDocument(counter)
                if attempts == 1 {
                    // Another client writes the document the transaction read.
                    let other = DispatchSemaphore(value: 0)
                    writer.document("t/counter").setData(["n": 100]) { _ in other.signal() }
                    other.wait()
                }
                let n = (snap.get("n") as? Int64) ?? 0
                transaction.updateData(["n": n + 1], forDocument: counter)
                return n + 1
            } catch {
                errorPointer?.pointee = error as NSError
                return nil
            }
        }
        return ["returned": norm(result), "retried": attempts > 1, "now": try await read(counter)]
    }
    await step("a batch after the transactions") {
        let batch = db.batch()
        batch.updateData(["n": FieldValue.increment(Int64(10))], forDocument: counter)
        try await batch.commit()
        return try await read(counter)
    }
}

// --- listen ---------------------------------------------------------------------------------

/// The other client: the listen section writes through it, and a transaction meets its write.
let writer = instance("writer")

final class Log: @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [(String, [String: Any])] = []
    private var last = Date()

    func record(_ listener: String, _ entry: [String: Any]) {
        lock.withLock {
            entries.append((listener, entry))
            last = Date()
        }
    }

    /// Waits until no snapshot came for 300 ms (at least 150 ms), then takes what came.
    func quiet() async -> [(String, [String: Any])] {
        try? await Task.sleep(nanoseconds: 150_000_000)
        while lock.withLock({ Date().timeIntervalSince(last) }) < 0.3 {
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        return lock.withLock {
            let taken = entries
            entries = []
            return taken
        }
    }
}

func listen() async {
    let log = Log()
    let cities = writer.collection("cities")
    var steps: [Any] = []
    func listenStep(_ name: String, _ body: () async throws -> Void) async {
        var entry: [String: Any] = ["step": name]
        do { try await body() } catch { entry["error"] = errorOf(error) }
        var listeners: [String: [Any]] = [:]
        for (listener, snapshot) in await log.quiet() { listeners[listener, default: []].append(snapshot) }
        entry["listeners"] = listeners
        steps.append(entry)
    }
    func query(_ name: String, _ q: Query) -> ListenerRegistration {
        q.addSnapshotListener { snap, error in
            guard let snap else {
                log.record(name, ["error": errorOf(error!)])
                return
            }
            log.record(name, [
                "docs": snap.documents.map { [$0.documentID, norm($0.data())] },
                "changes": snap.documentChanges.map { change -> [Any] in
                    let type = ["added", "modified", "removed"][Int(change.type.rawValue)]
                    // An index the document does not have is NSNotFound; -1 as the web SDK has.
                    let index = { (i: UInt) in i == UInt(NSNotFound) ? -1 : Int(i) }
                    return [type, change.document.documentID, index(change.oldIndex), index(change.newIndex)]
                },
            ])
        }
    }
    func document(_ name: String, _ ref: DocumentReference) -> ListenerRegistration {
        ref.addSnapshotListener { snap, error in
            guard let snap else {
                log.record(name, ["error": errorOf(error!)])
                return
            }
            log.record(name, ["exists": snap.exists, "data": snap.exists ? norm(snap.data()) : NSNull()])
        }
    }

    await listenStep("seed") {
        let batch = writer.batch()
        batch.setData(["pop": 860, "state": "CA"], forDocument: cities.document("SF"))
        batch.setData(["pop": 3900, "state": "CA"], forDocument: cities.document("LA"))
        batch.setData(["pop": 9000, "state": NSNull()], forDocument: cities.document("TOK"))
        try await batch.commit()
    }
    var registrations: [ListenerRegistration] = []
    await listenStep("attach listeners") {
        let mine = db.collection("cities")
        registrations.append(query("all", mine))
        registrations.append(query("pop > 1000", mine.whereField("pop", isGreaterThan: 1000)))
        registrations.append(query("top 2 by pop", mine.order(by: "pop", descending: true).limit(to: 2)))
        registrations.append(query("CA by pop", mine.whereField("state", isEqualTo: "CA").order(by: "pop")))
        registrations.append(query("group landmarks", db.collectionGroup("landmarks")))
        registrations.append(document("doc SF", db.document("cities/SF")))
        registrations.append(document("doc NY", db.document("cities/NY")))
    }
    await listenStep("add NY (enters several queries)") {
        try await cities.document("NY").setData(["pop": 8300, "state": "NY"])
    }
    await listenStep("update SF (stays in all)") { try await cities.document("SF").updateData(["pop": 870]) }
    await listenStep("update LA below 1000 (leaves pop > 1000, moves in order)") {
        try await cities.document("LA").updateData(["pop": 999])
    }
    await listenStep("update TOK above all (changes the top 2)") {
        try await cities.document("TOK").updateData(["pop": 9500])
    }
    await listenStep("no-op write") { try await cities.document("SF").setData(["pop": 870, "state": "CA"]) }
    await listenStep("write to another collection") { try await writer.document("other/x").setData(["n": 1]) }
    await listenStep("batch: delete TOK, add BJ, change NY state") {
        let batch = writer.batch()
        batch.deleteDocument(cities.document("TOK"))
        batch.setData(["pop": 21500, "state": NSNull()], forDocument: cities.document("BJ"))
        batch.updateData(["state": "CA"], forDocument: cities.document("NY"))
        try await batch.commit()
    }
    await listenStep("add a landmark") {
        try await writer.document("cities/SF/landmarks/gg").setData(["name": "Golden Gate"])
    }
    await listenStep("server timestamp") {
        try await cities.document("SF").updateData(["at": FieldValue.serverTimestamp()])
    }
    await listenStep("idle for 95 seconds") { try await Task.sleep(nanoseconds: 95_000_000_000) }
    await listenStep("update SF after the idle time") { try await cities.document("SF").updateData(["pop": 880]) }
    await listenStep("delete NY") { try await cities.document("NY").delete() }
    await listenStep("detach") { for registration in registrations { registration.remove() } }
    out = steps
}

// --- running --------------------------------------------------------------------------------

let finished = DispatchSemaphore(value: 0)
Task {
    switch section {
    case "documents": await documents()
    case "queries": await queries()
    case "transactions": await transactions()
    default: await listen()
    }
    finished.signal()
}
finished.wait()
let json = try JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
FileHandle.standardOutput.write(json)
FileHandle.standardOutput.write("\n".data(using: .utf8)!)
exit(0)
