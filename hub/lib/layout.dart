// Layouts are pure functions (plan 9.3): arrange(tiles, area, params) → rect per tile.
import 'dart:math' as math;
import 'dart:ui';

/// `grid`: picks the column count that maximizes the smallest scale of any tile (given aspect ratios),
/// then fills rows left to right; each cell is `gap` apart, tiles letterbox inside their cell.
List<Rect> arrangeGrid(List<double> aspects, Size area, {double gap = 6}) {
  final n = aspects.length;
  if (n == 0 || area.isEmpty) return [];
  var bestCols = 1;
  var bestScale = -1.0;
  for (var cols = 1; cols <= n; cols++) {
    final rows = (n / cols).ceil();
    final cw = (area.width - gap * (cols - 1)) / cols;
    final ch = (area.height - gap * (rows - 1)) / rows;
    if (cw <= 0 || ch <= 0) continue;
    // Scale of each tile = min(cw / w, ch / h) with h = 1, w = aspect; the grid is as good as its worst tile.
    var worst = double.infinity;
    for (final a in aspects) {
      worst = math.min(worst, math.min(cw / a, ch));
    }
    if (worst > bestScale + 1e-9) {
      bestScale = worst;
      bestCols = cols;
    }
  }
  final cols = bestCols;
  final rows = (n / cols).ceil();
  final cw = (area.width - gap * (cols - 1)) / cols;
  final ch = (area.height - gap * (rows - 1)) / rows;
  return [
    for (var i = 0; i < n; i++)
      Rect.fromLTWH((i % cols) * (cw + gap), (i ~/ cols) * (ch + gap), cw, ch),
  ];
}

/// Fits content of `aspect` (w/h) into `cell`, centred (plan 9.2 `scale`).
Rect letterbox(Rect cell, double aspect) {
  if (aspect <= 0) return cell;
  var w = cell.width;
  var h = w / aspect;
  if (h > cell.height) {
    h = cell.height;
    w = h * aspect;
  }
  return Rect.fromLTWH(cell.left + (cell.width - w) / 2, cell.top + (cell.height - h) / 2, w, h);
}
