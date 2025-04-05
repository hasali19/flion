import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

const _platformViewsChannel = MethodChannel('flion/platform_views');

class FlionPlatformView extends StatefulWidget {
  final String type;
  final dynamic args;

  const FlionPlatformView({super.key, required this.type, this.args});

  @override
  State<FlionPlatformView> createState() => _FlionPlatformViewState();
}

class _FlionPlatformViewState extends State<FlionPlatformView> {
  late final int _id;

  bool _isInit = false;

  @override
  void initState() {
    super.initState();

    _id = platformViewsRegistry.getNextPlatformViewId();

    _platformViewsChannel
        .invokeMethod('create', {
          'id': _id,
          'type': widget.type,
          'args': widget.args,
        })
        .then((value) {
          setState(() {
            _isInit = true;
          });
        });
  }

  @override
  void dispose() {
    super.dispose();
    _platformViewsChannel.invokeMethod('destroy', {'id': _id});
  }

  @override
  Widget build(BuildContext context) {
    if (_isInit) {
      return _FlionPlatformViewImpl(viewId: _id);
    } else {
      return Container();
    }
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
