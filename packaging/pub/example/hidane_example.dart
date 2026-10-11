// Most projects run hidane from the command line:
//
//     dart pub global activate hidane
//     hidane exec -- firebase emulators:start --only firestore
//
// The library starts it from Dart code too, for example from a test harness. The first call
// downloads the release binary for this platform and checks its SHA-256; later calls reuse it.
import 'dart:io';

import 'package:hidane/hidane.dart';

Future<void> main() async {
  // Prints `hidane <version>`; any other arguments are passed the same way, such as
  // ['--host', '127.0.0.1', '--port', '8080'] to start the emulator.
  final code = await runHidane(['--version']);
  exit(code);
}
