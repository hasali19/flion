import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

const _platformViewsChannel = MethodChannel('flion/platform_views');

class FlionPlatformViewController extends ChangeNotifier {
  late final int id;

  bool _isInit = false;
  bool get isInit => _isInit;

  FlionPlatformViewController() {
    id = platformViewsRegistry.getNextPlatformViewId();
  }

  Future<dynamic> init({required String type, dynamic args}) async {
    if (_isInit) {
      throw Exception('already initialised');
    }
    final result = await _platformViewsChannel.invokeMethod('create', {
      'id': id,
      'type': type,
      'args': args,
    });
    _isInit = true;
    return result;
  }

  @override
  void dispose() {
    super.dispose();
    _platformViewsChannel.invokeMethod('destroy', {'id': id});
  }
}

class FlionPlatformView extends StatefulWidget {
  final FlionPlatformViewController controller;

  const FlionPlatformView({super.key, required this.controller});

  @override
  State<FlionPlatformView> createState() => _FlionPlatformViewState();
}

class _FlionPlatformViewState extends State<FlionPlatformView> {
  @override
  void dispose() {
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    return ListenableBuilder(
      listenable: controller,
      builder: (context, child) {
        if (controller.isInit) {
          return _FlionPlatformViewImpl(viewId: controller.id);
        } else {
          return Container();
        }
      },
    );
  }
}

class _FlionPlatformViewImpl extends LeafRenderObjectWidget {
  final int viewId;

  const _FlionPlatformViewImpl({required this.viewId});

  @override
  RenderObject createRenderObject(BuildContext context) {
    return _PlatformViewRenderBox(viewId: viewId);
  }
}

class _PlatformViewRenderBox extends RenderBox {
  final int viewId;

  _PlatformViewRenderBox({required this.viewId});

  @override
  bool get sizedByParent => true;

  @override
  bool get alwaysNeedsCompositing => true;

  @override
  bool get isRepaintBoundary => true;

  @override
  @protected
  Size computeDryLayout(covariant BoxConstraints constraints) {
    return constraints.biggest;
  }

  @override
  void paint(PaintingContext context, Offset offset) {
    context.addLayer(PlatformViewLayer(rect: offset & size, viewId: viewId));
  }
}
