// Flutter Web scenarios for comparing emulators (results/flutter-web-*.json).
//
// cloud_firestore on the web drives the firebase JS SDK that firebase_core_web loads from
// gstatic.com, so everything below goes over WebChannel (listeners, reads, writes, batches) and
// REST (transactions, aggregations).
//
// Build it outside the repository: `flutter create --platforms web --empty app`, copy pubspec.yaml
// and lib/main.dart over the generated ones, then `flutter build web` and serve build/web (e.g.
// with `python3 -m http.server`). Open the page with `?port=<emulator port>` (the emulator on
// 127.0.0.1) in a browser, once per emulator, each started fresh. When it is done,
// `window.hidaneTranscript` holds the transcript as JSON and `window.hidaneDone` is true; each
// entry is also printed to the console. No time from the clock and no generated IDs end up in the
// transcript.
import 'dart:convert';
import 'dart:js_interop';
import 'dart:js_interop_unsafe';
import 'dart:typed_data';

import 'package:cloud_firestore/cloud_firestore.dart';
import 'package:firebase_core/firebase_core.dart';
import 'package:flutter/material.dart';

final List<Object?> transcript = [];
final ValueNotifier<String> status = ValueNotifier('running');

Future<void> main() async {
  runApp(const App());
  final port = int.parse(Uri.base.queryParameters['port'] ?? '8080');
  await Firebase.initializeApp(
    options: const FirebaseOptions(
      apiKey: 'demo-api-key',
      appId: '1:000000000000:web:0000000000000000',
      messagingSenderId: '000000000000',
      projectId: 'demo-hidane',
    ),
  );
  final db = FirebaseFirestore.instance;
  db.useFirestoreEmulator('127.0.0.1', port);
  // The firebase JS SDK that firebase_core_web loaded, and from where.
  final core = globalContext.getProperty<JSObject?>('firebase_core'.toJS);
  final resources = globalContext
      .getProperty<JSObject>('performance'.toJS)
      .callMethod<JSArray<JSObject>>('getEntriesByType'.toJS, 'resource'.toJS)
      .toDart;
  log('versions', {
    'firebaseJsSdk': core?.getProperty<JSString?>('SDK_VERSION'.toJS)?.toDart,
    'scripts': [
      for (final r in resources) r.getProperty<JSString>('name'.toJS).toDart,
    ].where((url) => url.contains('/firebasejs/')).toList()..sort(),
  });
  await run(db);
  globalContext.setProperty(
    'hidaneTranscript'.toJS,
    const JsonEncoder.withIndent(' ').convert(transcript).toJS,
  );
  globalContext.setProperty('hidaneDone'.toJS, true.toJS);
  status.value = 'done: ${transcript.length} entries';
}

class App extends StatelessWidget {
  const App({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
    home: Scaffold(
      body: Center(
        child: ValueListenableBuilder<String>(
          valueListenable: status,
          builder: (context, value, _) => Text(value),
        ),
      ),
    ),
  );
}

void log(String step, Object? value) {
  final entry = {'step': step, 'value': value};
  transcript.add(entry);
  // ignore: avoid_print
  print('hidane ${jsonEncode(entry)}');
}

Future<void> pause() => Future<void>.delayed(const Duration(milliseconds: 400));

// Runs one step and records its result or its error.
Future<void> step(String name, Future<Object?> Function() body) async {
  try {
    log(name, await body());
  } on FirebaseException catch (e) {
    log(name, {
      'error': {'code': e.code, 'message': e.message},
    });
  } catch (e) {
    log(name, {
      'error': {'type': e.runtimeType.toString(), 'text': e.toString()},
    });
  }
}

// Firestore values as JSON. Server timestamps are recorded by type only.
Object? enc(Object? v, [String? key]) => switch (v) {
  null || bool() || num() || String() => v,
  Timestamp() when key == 'updatedAt' => 'server timestamp',
  Timestamp t => {
    'timestamp': [t.seconds, t.nanoseconds],
  },
  GeoPoint g => {
    'geoPoint': [g.latitude, g.longitude],
  },
  Blob b => {'bytes': b.bytes.toList()},
  DocumentReference r => {'reference': r.path},
  VectorValue x => {'vector': x.toArray()},
  List l => [for (final e in l) enc(e)],
  Map m => {for (final e in m.entries) '${e.key}': enc(e.value, '${e.key}')},
  _ => {'unknown': v.runtimeType.toString()},
};

Map<String, Object?> doc(DocumentSnapshot<Map<String, dynamic>> s) => {
  'id': s.id,
  'exists': s.exists,
  'data': enc(s.data()),
  'fromCache': s.metadata.isFromCache,
  'pending': s.metadata.hasPendingWrites,
};

Map<String, Object?> query(QuerySnapshot<Map<String, dynamic>> s) => {
  'docs': [
    for (final d in s.docs) {'id': d.id, 'data': enc(d.data())},
  ],
  'changes': [
    for (final c in s.docChanges)
      [c.type.name, c.doc.id, c.oldIndex, c.newIndex],
  ],
  'fromCache': s.metadata.isFromCache,
  'pending': s.metadata.hasPendingWrites,
};

List<String> ids(QuerySnapshot<Map<String, dynamic>> s) => [
  for (final d in s.docs) d.id,
];

Future<void> run(FirebaseFirestore db) async {
  final alpha = db.doc('docs/alpha');
  final items = db.collection('items');

  // Documents: every value type, a read back, transforms, a merge, a delete.
  await step('set', () async {
    await alpha.set({
      'string': 'a',
      'int': 1,
      'double': 1.5,
      'bool': true,
      'null': null,
      'timestamp': Timestamp(1700000000, 123456789),
      'geoPoint': const GeoPoint(35.68, 139.76),
      'bytes': Blob(Uint8List.fromList([0, 1, 254, 255])),
      'reference': db.doc('docs/beta'),
      'list': [
        1,
        'two',
        {'three': 3},
      ],
      'map': {
        'k': 'v',
        'nested': {'deep': true},
      },
      'vector': const VectorValue([1.0, 2.5]),
    });
    return 'ok';
  });
  await step('get', () async => doc(await alpha.get()));
  await step(
    'get from server',
    () async => doc(await alpha.get(const GetOptions(source: Source.server))),
  );
  await step('update with transforms', () async {
    await alpha.update({
      'int': FieldValue.increment(2),
      'double': FieldValue.increment(0.25),
      'list': FieldValue.arrayUnion(['four', 1]),
      'map.k': 'w',
      'bool': FieldValue.delete(),
      'updatedAt': FieldValue.serverTimestamp(),
    });
    return doc(await alpha.get());
  });
  await step('array remove', () async {
    await alpha.update({
      'list': FieldValue.arrayRemove(['two']),
    });
    return enc((await alpha.get()).data()?['list']);
  });
  await step('set with merge', () async {
    await alpha.set({
      'merged': 'm',
      'map': {'added': 1},
    }, SetOptions(merge: true));
    return doc(await alpha.get());
  });
  await step('set with mergeFields', () async {
    await alpha.set({
      'string': 'b',
      'ignored': 1,
    }, SetOptions(mergeFields: ['string']));
    return enc((await alpha.get()).data());
  });
  await step('delete', () async {
    await db.doc('docs/beta').set({'x': 1});
    await db.doc('docs/beta').delete();
    return doc(await db.doc('docs/beta').get());
  });
  await step(
    'get a missing document',
    () async => doc(await db.doc('docs/missing').get()),
  );
  await step('add (generated ID)', () async {
    final ref = await db.collection('added').add({'n': 1});
    final read = await ref.get();
    return {'idLength': ref.id.length, 'data': enc(read.data())};
  });

  // Queries.
  await step('seed', () async {
    final batch = db.batch();
    for (var i = 1; i <= 6; i++) {
      batch.set(items.doc('i$i'), {
        'n': i,
        'group': i.isEven ? 'even' : 'odd',
        'tags': [if (i % 3 == 0) 'three', if (i > 4) 'big'],
      });
    }
    await batch.commit();
    return 'ok';
  });
  await step(
    'where == orderBy desc',
    () async => ids(
      await items
          .where('group', isEqualTo: 'even')
          .orderBy('n', descending: true)
          .get(),
    ),
  );
  await step(
    'range with limit',
    () async => ids(
      await items
          .where('n', isGreaterThanOrEqualTo: 2)
          .where('n', isLessThan: 5)
          .orderBy('n')
          .limit(2)
          .get(),
    ),
  );
  await step(
    'whereIn',
    () async => ids(await items.where('n', whereIn: [1, 3, 9]).get()),
  );
  await step(
    'whereNotIn',
    () async => ids(await items.where('n', whereNotIn: [1, 3]).get()),
  );
  await step(
    'isNotEqualTo',
    () async => ids(await items.where('group', isNotEqualTo: 'odd').get()),
  );
  await step(
    'arrayContains',
    () async => ids(await items.where('tags', arrayContains: 'three').get()),
  );
  await step(
    'arrayContainsAny',
    () async => ids(
      await items
          .where('tags', arrayContainsAny: ['big', 'three'])
          .orderBy('n')
          .get(),
    ),
  );
  await step(
    'Filter.or',
    () async => ids(
      await items
          .where(
            Filter.or(
              Filter('n', isEqualTo: 1),
              Filter('tags', arrayContains: 'big'),
            ),
          )
          .get(),
    ),
  );
  await step(
    'limitToLast',
    () async => ids(await items.orderBy('n').limitToLast(2).get()),
  );
  await step(
    'startAfter / endAt',
    () async => ids(await items.orderBy('n').startAfter([2]).endAt([4]).get()),
  );
  await step(
    'documentId',
    () async =>
        ids(await items.where(FieldPath.documentId, isGreaterThan: 'i4').get()),
  );
  await step('collectionGroup', () async {
    await db.doc('items/i1/notes/x').set({'n': 1});
    await db.doc('docs/alpha/notes/y').set({'n': 2});
    return [
      for (final d
          in (await db.collectionGroup('notes').orderBy('n').get()).docs)
        d.reference.path,
    ];
  });
  await step('count, sum, average', () async {
    final a = await items.aggregate(count(), sum('n'), average('n')).get();
    return {
      'count': a.count,
      'sum': a.getSum('n'),
      'average': a.getAverage('n'),
    };
  });
  await step(
    'count with a filter',
    () async =>
        (await items.where('group', isEqualTo: 'odd').count().get()).count,
  );

  // Listeners: a query (an add, a change, a move, a removal) and a document with metadata changes.
  final events = <Object?>[];
  final docEvents = <Object?>[];
  final querySub = items
      .where('group', isEqualTo: 'odd')
      .orderBy('n')
      .snapshots()
      .listen(
        (s) => events.add(query(s)),
        onError: (Object e) => events.add({'error': '$e'}),
      );
  final docSub = items
      .doc('i7')
      .snapshots(includeMetadataChanges: true)
      .listen(
        (s) => docEvents.add(doc(s)),
        onError: (Object e) => docEvents.add({'error': '$e'}),
      );
  await pause();
  await step('listen: add', () async {
    await items.doc('i7').set({'n': 7, 'group': 'odd', 'tags': []});
    await pause();
    return 'ok';
  });
  await step('listen: change', () async {
    await items.doc('i7').update({
      'tags': ['seven'],
    });
    await pause();
    return 'ok';
  });
  await step('listen: move', () async {
    await items.doc('i7').update({'n': 0});
    await pause();
    return 'ok';
  });
  await step('listen: leave the query', () async {
    await items.doc('i7').update({'group': 'even'});
    await pause();
    return 'ok';
  });
  await step('listen: delete', () async {
    await items.doc('i1').delete();
    await items.doc('i7').delete();
    await pause();
    return 'ok';
  });
  await querySub.cancel();
  await docSub.cancel();
  log('query snapshots', events);
  log('document snapshots', docEvents);

  // A batch, and transactions.
  await step('batch', () async {
    final batch = db.batch();
    batch.set(items.doc('b1'), {'n': 10, 'group': 'batch'});
    batch.update(items.doc('i2'), {'n': 20});
    batch.delete(items.doc('i4'));
    batch.set(items.doc('i3'), {'extra': true}, SetOptions(merge: true));
    await batch.commit();
    return [
      for (final d in (await items.orderBy(FieldPath.documentId).get()).docs)
        {'id': d.id, 'data': enc(d.data())},
    ];
  });
  await step('transaction: create', () async {
    final ref = db.doc('counters/c');
    await db.runTransaction((tx) async {
      final s = await tx.get(ref);
      tx.set(ref, {'count': s.exists ? (s.data()!['count'] as num) + 1 : 1});
    });
    return enc((await ref.get()).data());
  });
  await step('transaction: increment', () async {
    final ref = db.doc('counters/c');
    final seen = <Object?>[];
    await db.runTransaction((tx) async {
      final s = await tx.get(ref);
      final other = await tx.get(db.doc('items/i2'));
      seen.add([s.data()?['count'], other.data()?['n']]);
      tx.update(ref, {'count': (s.data()!['count'] as num) + 1});
    });
    return {'seen': seen, 'after': enc((await ref.get()).data())};
  });
  await step('transaction: delete', () async {
    final ref = db.doc('items/b1');
    await db.runTransaction((tx) async {
      await tx.get(ref);
      tx.delete(ref);
    });
    return (await ref.get()).exists;
  });

  // Errors.
  await step('error: update a missing document', () async {
    await db.doc('docs/missing').update({'n': 1});
    return 'no error';
  });
  await step('error: batch with a missing document', () async {
    final batch = db.batch();
    batch.set(items.doc('nb1'), {'n': 1});
    batch.update(db.doc('docs/missing'), {'n': 1});
    await batch.commit();
    return 'no error';
  });
  await step(
    'error: the batch wrote nothing',
    () async => (await items.doc('nb1').get()).exists,
  );
  await step('error: update a missing document in a transaction', () async {
    await db.runTransaction((tx) async {
      tx.update(db.doc('docs/missing'), {'n': 1});
    });
    return 'no error';
  });
  await step('error: reserved field name', () async {
    await db.doc('docs/reserved').set({'__reserved__': 1});
    return 'no error';
  });
  await step('error: invalid query', () async {
    await items.where('n', whereIn: []).get();
    return 'no error';
  });
  await step(
    'final state',
    () async => [
      for (final d in (await items.orderBy(FieldPath.documentId).get()).docs)
        {'id': d.id, 'data': enc(d.data())},
    ],
  );
}
