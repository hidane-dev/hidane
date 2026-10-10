import 'dart:io';

import 'package:hidane/hidane.dart';

Future<void> main(List<String> arguments) async {
  try {
    exit(await runHidane(arguments));
  } on Object catch (error) {
    stderr.writeln('hidane: $error');
    exit(1);
  }
}
