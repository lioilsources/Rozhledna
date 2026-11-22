// Rozhledna hub (T1.6): machines panel, pairing dialogs, grid of remote screens with input.
import 'package:flutter/material.dart';

import 'hub_api.dart';
import 'layout.dart';
import 'model.dart';
import 'tile_view.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  final model = HubModel(HubApi());
  model.start();
  runApp(HubApp(model: model));
}

class HubApp extends StatelessWidget {
  const HubApp({super.key, required this.model});
  final HubModel model;

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'Rozhledna',
        debugShowCheckedModeBanner: false,
        theme: ThemeData(
          colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF4EA1FF), brightness: Brightness.dark),
          useMaterial3: true,
        ),
        home: HubHome(model: model),
      );
}

class HubHome extends StatefulWidget {
  const HubHome({super.key, required this.model});
  final HubModel model;

  @override
  State<HubHome> createState() => _HubHomeState();
}

class _HubHomeState extends State<HubHome> {
  bool _panel = true, _stats = true;
  final _addr = TextEditingController();
  final Set<PairPrompt> _shown = {};

  HubModel get m => widget.model;

  @override
  void initState() {
    super.initState();
    m.addListener(_changed);
  }

  @override
  void dispose() {
    m.removeListener(_changed);
    super.dispose();
  }

  void _changed() {
    if (!mounted) return;
    setState(() {});
    for (final p in m.prompts.where((p) => !_shown.contains(p)).toList()) {
      _shown.add(p);
      _showPrompt(p);
    }
  }

  Future<void> _showPrompt(PairPrompt p) async {
    final code = '${p.sas.substring(0, 3)} ${p.sas.substring(3)}';
    final r = await showDialog<bool>(
      context: context,
      barrierDismissible: false,
      builder: (ctx) {
        // Close automatically when the agent answered (prompt removed from the model).
        void check() {
          if (!m.prompts.contains(p) && Navigator.of(ctx).canPop()) Navigator.of(ctx).pop();
        }

        m.addListener(check);
        return PopScope(
          onPopInvokedWithResult: (_, _) => m.removeListener(check),
          child: AlertDialog(
            title: Text(p.request == null ? 'Párování s „${p.name}“' : 'Neznámý stroj „${p.name}“'),
            content: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(p.request == null
                  ? 'Na stroji „${p.name}“ se objevil dotaz. Povol ho, pokud tam vidíš stejný kód:'
                  : 'Stroj tenhle hub zná, ale hub ho ne. Důvěřovat? Kód:'),
              const SizedBox(height: 16),
              Center(
                child: SelectableText(code,
                    style: const TextStyle(fontSize: 40, fontFeatures: [FontFeature.tabularFigures()], letterSpacing: 4)),
              ),
            ]),
            actions: p.request == null
                ? [TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Zavřít'))]
                : [
                    TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Odmítnout')),
                    FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Důvěřovat')),
                  ],
          ),
        );
      },
    );
    if (p.request != null) {
      m.answer(p, r ?? false);
    } else {
      m.dismissPrompt(p);
    }
  }

  Widget _agentsPanel() {
    final known = m.agents.values.toList();
    final fresh = m.discovered.values
        .where((d) => !known.any((a) => a.fingerprint.contains(d.fpShort) || d.addrs.contains(a.addr)))
        .toList();
    Color stateColor(String s) => switch (s) {
          'connected' => const Color(0xFF3DDC84),
          'pairing' => const Color(0xFF4EA1FF),
          _ => const Color(0xFFFFB020),
        };
    return Container(
      width: 280,
      color: const Color(0xFF15181D),
      child: ListView(padding: const EdgeInsets.all(12), children: [
        Text('Stroje', style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: 8),
        for (final a in known)
          Card(
            child: ListTile(
              dense: true,
              leading: Icon(Icons.circle, size: 12, color: stateColor(a.state)),
              title: Text(a.label),
              subtitle: Text([
                a.state == 'connected' ? '${a.sources.length} obrazovky' : a.state,
                if (!a.screenRecording) '✗ nahrávání obrazovky',
                if (!a.accessibility) '✗ zpřístupnění (ovládání)',
                if (a.locked) 'zamčeno',
                if (a.state == 'disconnected' && a.detail.isNotEmpty) a.detail,
              ].join(' · '), maxLines: 3, overflow: TextOverflow.ellipsis),
              trailing: IconButton(
                tooltip: 'Odebrat',
                icon: const Icon(Icons.close, size: 16),
                onPressed: () => m.removeAgent(a.id),
              ),
            ),
          ),
        if (fresh.isNotEmpty) ...[
          const SizedBox(height: 12),
          Text('V síti', style: Theme.of(context).textTheme.labelLarge),
          for (final d in fresh)
            ListTile(
              dense: true,
              leading: const Icon(Icons.desktop_mac_outlined, size: 18),
              title: Text(d.name),
              subtitle: Text(d.addrs.firstWhere((x) => !x.startsWith('127.'), orElse: () => d.addrs.first)),
              trailing: TextButton(
                onPressed: () => m.addAgent(d.addrs.firstWhere((x) => !x.startsWith('127.') && !x.startsWith('169.254.'),
                    orElse: () => d.addrs.first)),
                child: const Text('Přidat'),
              ),
            ),
        ],
        const SizedBox(height: 12),
        TextField(
          controller: _addr,
          decoration: const InputDecoration(labelText: 'Přidat ručně (adresa[:port])', isDense: true),
          onSubmitted: (v) {
            if (v.trim().isNotEmpty) m.addAgent(v.trim());
            _addr.clear();
          },
        ),
        const SizedBox(height: 16),
        SwitchListTile(
          dense: true,
          title: const Text('Statistiky na dlaždicích'),
          value: _stats,
          onChanged: (v) => setState(() => _stats = v),
        ),
        const SizedBox(height: 8),
        Text('Hub ${m.fingerprint.length > 23 ? m.fingerprint.substring(7, 23) : m.fingerprint}',
            style: Theme.of(context).textTheme.bodySmall),
        const SizedBox(height: 4),
        Text('Klik do dlaždice = fokus; klávesnice jde do fokusované dlaždice. Systémové zkratky (⌘Tab) zatím ne (T1.7).',
            style: Theme.of(context).textTheme.bodySmall),
      ]),
    );
  }

  @override
  Widget build(BuildContext context) {
    final tiles = m.tiles.values.toList()..sort((a, b) => a.id.compareTo(b.id));
    return Scaffold(
      backgroundColor: const Color(0xFF0B0D10),
      body: Row(children: [
        if (_panel) _agentsPanel(),
        Expanded(
          child: Stack(children: [
            if (tiles.isEmpty)
              Center(
                child: Text(
                    m.agents.isEmpty ? 'Přidej stroj vlevo (z mDNS nebo adresou).' : 'Čekám na obrazovky…',
                    style: const TextStyle(color: Colors.white54)),
              ),
            LayoutBuilder(builder: (context, c) {
              const pad = 8.0;
              final area = Size(c.maxWidth - 2 * pad, c.maxHeight - 2 * pad);
              final rects = arrangeGrid(tiles.map((t) => t.aspect).toList(), area);
              return Stack(children: [
                for (var i = 0; i < tiles.length; i++)
                  Positioned.fromRect(
                    rect: rects[i].shift(const Offset(pad, pad)),
                    child: TileWidget(key: ValueKey(tiles[i].id), model: m, tile: tiles[i], showStats: _stats),
                  ),
              ]);
            }),
            Positioned(
              left: 4,
              bottom: 4,
              child: IconButton(
                tooltip: _panel ? 'Skrýt panel' : 'Ukázat panel',
                icon: Icon(_panel ? Icons.chevron_left : Icons.chevron_right, color: Colors.white38),
                onPressed: () => setState(() => _panel = !_panel),
              ),
            ),
          ]),
        ),
      ]),
    );
  }
}
