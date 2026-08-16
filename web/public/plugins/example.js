// 示例外部插件:为消息文本追加「统计」前缀注释(演示 messageTransform 用法)
// 启用方式:修改 manifest.json 加入本文件;或直接引用 registerPlugin 全局。
(function () {
  if (typeof registerPlugin === 'function') {
    registerPlugin({
      id: 'example-stats',
      name: '示例插件(消息统计)',
      version: '0.1.0',
      description: '演示插件 API:在每条消息前追加字符数统计。',
      messageTransform: function (content, role) {
        return '【' + role + ' · ' + content.length + ' 字】' + content;
      },
    });
  }
})();
