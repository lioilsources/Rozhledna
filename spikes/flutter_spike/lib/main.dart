// T0.6 Flutter spike: one remote display as a Flutter Texture + latency overlay.
//
// Automation (env vars, read when the app starts):
//   RZ_SPIKE_ADDR=host:port  RZ_SPIKE_AUTOSTART=1  RZ_SPIKE_DURATION=10  RZ_SPIKE_CSV=/path/out.csv
//   RZ_SPIKE_MAX=2560x1440   RZ_SPIKE_FPS=60       RZ_SPIKE_QUIT=1 (quit after the duration)

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

const _channel = MethodChannel('rozhledna/spike');

void main() => runApp(const SpikeApp());

class SpikeApp extends StatelessWidget {
  const SpikeApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'Rozhledna Flutter spike',
        debugShowCheckedModeBanner: false,
        theme: ThemeData.dark(useMaterial3: true),
        home: const SpikePage(),
      );
}

class SpikePage extends StatefulWidget {
  const SpikePage({super.key});

  @override
  State<SpikePage> createState() => _SpikePageState();
}

class _SpikePageState extends State<SpikePage> {
  final _addr = TextEditingController(text: '127.0.0.1:47470');
  Map<String, String> _env = {};
  int? _textureId;
  String _overlay = '';
  String _report = '';
  Timer? _poll;
  double _aspect = 16 / 9;

  @override
  void initState() {
    super.initState();
    _init();
  }

  Future<void> _init() async {
    final env = await _channel.invokeMapMethod<String, String>('env') ?? {};
    setState(() => _env = env);
    if (env['RZ_SPIKE_ADDR'] != null) _addr.text = env['RZ_SPIKE_ADDR']!;
    if (env['RZ_SPIKE_AUTOSTART'] == '1') {
      await _start();
      final secs = double.tryParse(env['RZ_SPIKE_DURATION'] ?? '');
      if (secs != null) {
        Timer(Duration(milliseconds: (secs * 1000).round()), () async {
          await _stop();
          if (env['RZ_SPIKE_QUIT'] == '1') await _channel.invokeMethod('quit');
        });
      }
    }
  }

  Future<void> _start() async {
    final max = (_env['RZ_SPIKE_MAX'] ?? '3840x2160').split('x').map(int.parse).toList();
    _aspect = max[0] / max[1];
    try {
      final id = await _channel.invokeMethod<int>('start', {
        'addr': _addr.text,
        'maxW': max[0],
        'maxH': max[1],
        'fps': int.tryParse(_env['RZ_SPIKE_FPS'] ?? '') ?? 60,
      });
      setState(() {
        _textureId = id;
        _report = '';
      });
      _poll = Timer.periodic(const Duration(milliseconds: 250), (_) async {
        final s = await _channel.invokeMethod<String>('stats') ?? '';
        if (mounted) setState(() => _overlay = s);
        // Connection/subscribe failure: in automated mode end right away instead of waiting for the duration.
        if (s.contains('\nERROR:') && _env['RZ_SPIKE_AUTOSTART'] == '1') {
          await _stop();
          setState(() => _report = '$s\n$_report');
          if (_env['RZ_SPIKE_QUIT'] == '1') await _channel.invokeMethod('quit');
        }
      });
    } on PlatformException catch (e) {
      setState(() => _report = 'start failed: ${e.message}');
    }
  }

  Future<void> _stop() async {
    if (_poll == null) return; // not running (or already stopped after an error)
    _poll!.cancel();
    _poll = null;
    final r = await _channel.invokeMethod<String>('stop', {'csv': _env['RZ_SPIKE_CSV'] ?? ''}) ?? '';
    setState(() {
      _textureId = null;
      _report = r;
    });
  }

  @override
  Widget build(BuildContext context) {
    const mono = TextStyle(fontFamily: 'Menlo', fontSize: 12, color: Color(0xFFFFFF99));
    return Scaffold(
      backgroundColor: Colors.black,
      body: Column(children: [
        Padding(
          padding: const EdgeInsets.all(8),
          child: Row(children: [
            SizedBox(width: 220, child: TextField(controller: _addr, decoration: const InputDecoration(labelText: 'agent host:port', isDense: true))),
            const SizedBox(width: 8),
            FilledButton(onPressed: _textureId == null ? _start : null, child: const Text('Connect')),
            const SizedBox(width: 8),
            OutlinedButton(onPressed: _textureId != null ? _stop : null, child: const Text('Stop + report')),
          ]),
        ),
        Expanded(
          child: Stack(children: [
            if (_textureId != null)
              Center(child: AspectRatio(aspectRatio: _aspect, child: Texture(textureId: _textureId!, filterQuality: FilterQuality.medium))),
            Positioned(
              left: 8,
              top: 8,
              child: Container(
                color: const Color(0x99000000),
                padding: const EdgeInsets.all(6),
                child: Text(_textureId != null ? _overlay : _report, style: mono),
              ),
            ),
          ]),
        ),
      ]),
    );
  }
}
