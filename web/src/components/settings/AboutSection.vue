<script setup lang="ts">
// 设置区:关于 + 文字教程。
// 关于文案由项目方提供(版本/作者/代传/协议/更新内容),原样呈现;
// 教程为面向新用户的功能速览,与实现保持同步(改功能时同步此文件)。
// 与 UiSection 同级的纯展示分区,无业务状态依赖。
const props = withDefaults(defineProps<{
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

/** 更新内容:与项目方给定的文案逐条一致 */
const changelog: string[] = [
  '加入了任务模式,现在 kedai 可以帮你完成文字任务了',
  '优化角色扮演模式对酒馆生态的兼容性',
  '加入向量化记忆系统',
  '提升稳定性',
];
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme blue" /> 关于</div>

    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <b>kedai 0.2.1 beta Android</b>
        <span>作者:十七凌云(Sata1949)</span>
        <span>代传:TK(SpaceRelay)</span>
        <span>遵循 MIT 协议</span>
        <span>感谢您在百忙之中支持此软件。</span>
      </div>
    </div>

    <div class="sv-field-label sub">更新内容</div>
    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <span v-for="(item, i) in changelog" :key="i">{{ i + 1 }}. {{ item }}</span>
      </div>
    </div>

    <div class="sv-field-label sub">使用教程</div>

    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <b>角色扮演模式</b>
        <span>在左侧角色列表选择角色卡 → 顶栏「开场」可切换多条开场白。</span>
        <span>若卡片界面只显示文字、没有样式:打开顶栏「HTML」开关(按角色卡记忆)。</span>
        <span>若界面能显示但下拉、按钮点了没反应:开启顶栏「JS」授权(会先弹出风险确认,仅对当前角色与当前脚本版本生效)。</span>
        <span>多个「开场」= 多条开场白,切换会清空当前会话并重新开始。</span>
      </div>
    </div>

    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <b>任务模式</b>
        <span>顶栏切换到「任务」,输入目标即可;支持 solo / multi / plan / team / custom 等多种执行模式(见「执行流程」)。</span>
        <span>复杂目标建议用 plan 或 team:plan 先出计划、确认后执行;team 会分派子目标并做审计。</span>
        <span>执行过程与工具调用可在右侧 Agent 面板查看。</span>
      </div>
    </div>

    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <b>记忆与世界书</b>
        <span>世界书条目按关键词自动激发;角色卡内嵌世界书随卡导入。</span>
        <span>「向量化模型」开启后可做语义召回,记忆库面板可查看与蒸馏长期记忆。</span>
      </div>
    </div>

    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <b>提示词与授权</b>
        <span>「Agent 设置」可编辑角色扮演与任务两套提示词;「提示词注入」配置常驻楼层。</span>
        <span>「授权」三档(严格 / 宽松 / 放行)决定工具执行前的确认策略,涉及系统路径写删恒需授权。</span>
      </div>
    </div>

    <div class="sv-data-row" style="align-items: flex-start">
      <div class="info">
        <b>快捷键与输入</b>
        <span>Enter 发送,Shift+Enter 换行;输入「/」触发命令联想。</span>
      </div>
    </div>
  </div>
</template>
