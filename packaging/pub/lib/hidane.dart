/// Downloads and runs [hidane](https://github.com/hidane-dev/hidane), a Firestore emulator
/// without Java.
///
/// The first run downloads the release archive for this platform from GitHub Releases, checks
/// it against the release's `sha256sums.txt` and keeps the binary in a cache directory; later
/// runs start it directly.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:ffi' show Abi;
import 'dart:io';
import 'dart:typed_data' show BytesBuilder;

import 'package:crypto/crypto.dart';

/// The hidane release this launcher runs.
const String hidaneVersion = '0.1.0-rc.2';

const String _releases =
    'https://github.com/hidane-dev/hidane/releases/download';

/// Runs hidane with [arguments], sharing this process's standard streams, and returns its exit
/// code (128 + n when a signal n ended it).
///
/// `HIDANE_CACHE_DIR` overrides the cache directory, and `HIDANE_RELEASES_URL` the location of
/// the release files (a mirror holding `v<version>/<file>`).
Future<int> runHidane(List<String> arguments) async {
  final binary = await _binary();
  final process = await Process.start(binary.path, arguments,
      mode: ProcessStartMode.inheritStdio);
  // Stay alive until hidane exits. Signals sent to this process alone are passed on; from a
  // terminal hidane receives Ctrl-C itself. On Windows Ctrl-C reaches both processes anyway.
  final signals = [
    ProcessSignal.sigint,
    if (!Platform.isWindows) ...[ProcessSignal.sigterm, ProcessSignal.sighup]
  ];
  final subscriptions = [
    for (final signal in signals)
      signal.watch().listen((s) {
        if (!Platform.isWindows) process.kill(s);
      }),
  ];
  final code = await process.exitCode;
  for (final subscription in subscriptions) {
    await subscription.cancel();
  }
  return code < 0 ? 128 - code : code;
}

/// Why hidane could not be started.
class HidaneException implements Exception {
  /// What went wrong.
  final String message;

  /// An exception saying [message].
  HidaneException(this.message);

  @override
  String toString() => message;
}

String? _target() => switch (Abi.current()) {
      Abi.macosArm64 => 'aarch64-apple-darwin',
      Abi.macosX64 => 'x86_64-apple-darwin',
      Abi.linuxArm64 => 'aarch64-unknown-linux-musl',
      Abi.linuxX64 => 'x86_64-unknown-linux-musl',
      Abi.windowsX64 => 'x86_64-pc-windows-msvc',
      _ => null,
    };

Directory _cacheRoot() {
  final env = Platform.environment;
  final override = env['HIDANE_CACHE_DIR'];
  if (override != null && override.isNotEmpty) return Directory(override);
  if (Platform.isWindows) {
    return Directory(
        '${env['LOCALAPPDATA'] ?? env['USERPROFILE']}\\hidane\\cache');
  }
  if (Platform.isMacOS)
    return Directory('${env['HOME']}/Library/Caches/hidane');
  final xdg = env['XDG_CACHE_HOME'];
  return Directory(xdg != null && xdg.isNotEmpty
      ? '$xdg/hidane'
      : '${env['HOME']}/.cache/hidane');
}

Future<File> _binary() async {
  final target = _target();
  if (target == null) {
    throw HidaneException('hidane has no binary for ${Abi.current()}; '
        'other platforms can build it from source: cargo install hidane');
  }
  final exe = Platform.isWindows ? 'hidane.exe' : 'hidane';
  final dir = Directory('${_cacheRoot().path}/$hidaneVersion/$target');
  final binary = File('${dir.path}/$exe');
  if (binary.existsSync()) return binary;

  final archive =
      'hidane-$hidaneVersion-$target${Platform.isWindows ? '.zip' : '.tar.gz'}';
  final base = Platform.environment['HIDANE_RELEASES_URL'] ?? _releases;
  stderr.writeln('hidane: downloading $archive');
  final sums = utf8
      .decode(await _get(Uri.parse('$base/v$hidaneVersion/sha256sums.txt')));
  final expected = LineSplitter.split(sums)
      .map((line) => line.trim().split(RegExp(r'\s+')))
      .firstWhere((fields) => fields.length == 2 && fields[1] == archive,
          orElse: () => throw HidaneException(
              '$archive is not listed in sha256sums.txt'))[0];
  final bytes = await _get(Uri.parse('$base/v$hidaneVersion/$archive'));
  final actual = sha256.convert(bytes).toString();
  if (actual != expected) {
    throw HidaneException(
        '$archive has SHA-256 $actual, sha256sums.txt says $expected');
  }

  dir.createSync(recursive: true);
  final work = dir.createTempSync('download-');
  try {
    final file = File('${work.path}/$archive')..writeAsBytesSync(bytes);
    // tar ships with macOS, Linux and Windows 10 and later, where it also reads .zip.
    final tar = await Process.run('tar',
        [Platform.isWindows ? '-xf' : '-xzf', file.path, '-C', work.path]);
    if (tar.exitCode != 0)
      throw HidaneException('could not unpack $archive: ${tar.stderr}');
    File('${work.path}/hidane-$hidaneVersion-$target/$exe')
        .renameSync(binary.path);
  } finally {
    work.deleteSync(recursive: true);
  }
  return binary;
}

Future<List<int>> _get(Uri uri) async {
  final client = HttpClient();
  try {
    final response = await (await client.getUrl(uri)).close();
    if (response.statusCode != HttpStatus.ok) {
      throw HttpException('HTTP ${response.statusCode}', uri: uri);
    }
    final bytes = BytesBuilder(copy: false);
    await response.forEach(bytes.add);
    return bytes.takeBytes();
  } finally {
    client.close();
  }
}
