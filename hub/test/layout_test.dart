import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:rozhledna_hub/layout.dart';

void main() {
  test('one tile fills the area', () {
    expect(arrangeGrid([16 / 9], const Size(1600, 900), gap: 0), [const Rect.fromLTWH(0, 0, 1600, 900)]);
  });

  test('two 16:9 tiles on a wide area go side by side', () {
    final r = arrangeGrid([16 / 9, 16 / 9], const Size(3200, 900), gap: 0);
    expect(r, [const Rect.fromLTWH(0, 0, 1600, 900), const Rect.fromLTWH(1600, 0, 1600, 900)]);
  });

  test('two 16:9 tiles on a 16:9 area stack or split, whichever is larger', () {
    // Side by side: each 800 wide → scale 450 px tall; stacked: each 450 tall, 800 wide → same; the first wins.
    final r = arrangeGrid([16 / 9, 16 / 9], const Size(1600, 900), gap: 0);
    expect(r.length, 2);
    expect(r[0].width * r[0].height, closeTo(800 * 900, 1e-6));
  });

  test('four tiles make a 2×2 grid with gaps', () {
    final r = arrangeGrid(List.filled(4, 16 / 9), const Size(1606, 906), gap: 6);
    expect(r[3], const Rect.fromLTWH(806, 456, 800, 450));
  });

  test('letterbox centres and keeps aspect', () {
    final l = letterbox(const Rect.fromLTWH(0, 0, 1000, 1000), 2);
    expect(l, const Rect.fromLTWH(0, 250, 1000, 500));
  });
}
