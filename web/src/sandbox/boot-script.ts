// boot-script.ts — 沙箱 boot 文档字符串生成(sandboxScript:变量快照/兼容 API/jQuery 子集/事件总线等沙箱内代码的代码生成)
import type { MvuVariables } from '../mvu/variables';
import { CHANNEL, cloneData, type ContainerGeo, type ScriptSegment } from './protocol';

/**
 * 沙箱 iframe 内容。除基本变量/通信外,内联补全 MagVarUpdate 生态脚本依赖的兼容 API:
 *   - _ (lodash 子集 get/set,叶子 [新值,原因] 自动取 [0])
 *   - $ (jQuery 子集,经 RPC 转发到宿主容器操作 DOM)
 *   - waitGlobalInitialized / errorCatched
 *   - Mvu / getAllVariables / Dom
 * 沙箱是隔离窗口,看不到宿主 window 上的 installMvuGlobals,因此这里必须自带全部全局。
 *
 * code 两种形态:
 *   - string:单段(消息级),用户代码包在 async IIFE 里尾部执行,现行为不变;
 *   - ScriptSegment[]:多段(卡级合并单 realm),boot <script> 之后按序每段一个独立
 *     <script> 标签注入——独立标签天然共享 window 与全局词法环境(语义与 SillyTavern
 *     逐 <script> 加载一致),单段语法错误只废该段不拖垮其余段,段名经
 *     window.__kdScriptName 附到错误上报前缀([卡脚本 段名])。
 */
export function sandboxScript(
  code: string | ScriptSegment[],
  nonce: string,
  variables: MvuVariables,
  globals: Record<string, unknown> = {},
  storage: Record<string, string> = {},
  geo: ContainerGeo | undefined = undefined,
  idMap: Record<string, unknown> = {},
  lorebookName: string | null = null,
  sharedGlobals: Record<string, unknown> = {},
  /** 启动时的宿主表单控件快照(gatherControls):document.getElementById 门面读 select/input
   *  真值的同步数据源(事件回包也会带 controls,此处覆盖脚本 boot 后、首次事件前的裸读)。 */
  controls: Array<Record<string, unknown>> = [],
): string {
  const segments = typeof code === 'string' ? null : code;
  const escapedCode = segments === null ? (code as string).replace(/<\/script/gi, '<\\/script') : '';
  const initialData = JSON.stringify(cloneData(variables)).replace(/</g, '\\u003c');
  const initialGlobals = JSON.stringify(cloneData(globals)).replace(/</g, '\\u003c');
  const initialStorage = JSON.stringify(cloneData(storage)).replace(/</g, '\\u003c');
  const initialGeo = JSON.stringify(geo ?? null).replace(/</g, '\\u003c');
  const initialMirror = JSON.stringify(cloneData(idMap)).replace(/</g, '\\u003c');
  const initialControls = JSON.stringify(cloneData(controls)).replace(/</g, '\\u003c');
  const initialLorebook = JSON.stringify(lorebookName ?? null);
  const initialNonce = JSON.stringify(nonce);
  const initialShared = JSON.stringify(cloneData(sharedGlobals)).replace(/</g, '\\u003c');
  // 多段正文经 JSON 内联(< → < 转义双保险;段文本最终以 textContent 注入,不经 HTML 解析)
  const segmentsJson = JSON.stringify(
    (segments ?? []).map((s) => ({ name: String(s?.name ?? ''), code: String(s?.code ?? '') })),
  ).replace(/</g, '\\u003c');
  const runUserCode = segments === null
    // 单段(消息级):现行为不变,仅在 done 前挂共享桥首次 diff。
    // 额外在用户代码前注册 __kdEvalInline:直接 eval 定义在 IIFE 内,作用域链含用户代码的
    // function 声明与 const 绑定;inline 事件降级桥据此求值(onclick/onchange 里调用卡内函数)。
    ? `(async()=>{try{
const __kdInlineEval=function(__c,__e){var event=__e;return eval(__c);};
window.__kdEvalInline=__kdInlineEval;globalThis.__kdEvalInline=__kdInlineEval;
${escapedCode}
;await flush();__kdSharedStart();send('done')}catch(error){send('error',{message:error instanceof Error?(error.stack||error.message):String(error)})}})();`
    // 多段(卡级合并单 realm):逐段独立 <script> 注入(appendChild 同步执行,语义对齐
    // SillyTavern 逐 <script> 加载:共享 window 与全局词法环境,单段语法错误只废该段)。
    // 错误三路径(window error / unhandledrejection / harness finish)统一带段名前缀。
    : `window.__kdScriptName='';
const __kdSegPrefix=function(){return window.__kdScriptName?'[卡脚本 '+window.__kdScriptName+'] ':'';};
addEventListener('error',function(e){
  const em=(e&&e.error&&(e.error.stack||e.error.message))||(e&&e.message)||'未知错误';
  send('warn',{message:__kdSegPrefix()+em});
});
addEventListener('unhandledrejection',function(e){
  const r=e?e.reason:null;
  send('warn',{message:__kdSegPrefix()+'未处理的 Promise 拒绝: '+(r&&r.message?r.message:String(r))});
});
const __kdSegments=JSON.parse(${JSON.stringify(segmentsJson)});
for(let __i=0;__i<__kdSegments.length;__i++){
  const __seg=__kdSegments[__i];
  const __s=document.createElement('script');
  // 段开头标记当前段名,错误钩子据此加前缀;textContent 注入不触发 HTML 解析(无 </script> 闭合风险)
  __s.textContent='window.__kdScriptName='+JSON.stringify(String(__seg.name||''))+';\\n'+__seg.code;
  document.head.appendChild(__s);
}
window.__kdScriptName='';
(async()=>{try{await flush();__kdSharedStart();send('done')}catch(error){send('error',{message:__kdSegPrefix()+(error instanceof Error?(error.stack||error.message):String(error))})}})();`;
  return `'use strict';
const CHANNEL=${JSON.stringify(CHANNEL)}, NONCE=${initialNonce};
const variables=JSON.parse(${JSON.stringify(initialData)});
const __kdGlobals=JSON.parse(${JSON.stringify(initialGlobals)});
const __kdSavedLocal=JSON.parse(${JSON.stringify(initialStorage)});
// 主世界书名(角色卡内嵌 character_book.name;无则 null):TavernHelper 世界书 API 的同步数据源
const __KD_LOREBOOK__=${initialLorebook};
let seq=0;
const pending=new Map();
// 通信锚定真实父窗口:下文会给 window.parent 装门面(供作者脚本读取),send 不能绕经门面
const __kdSendTarget=parent;
const send=(type, extra={})=>__kdSendTarget.postMessage({channel:CHANNEL,nonce:NONCE,type,...extra}, '*');
const rpc=(op,...args)=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});send('rpc',{id,op,args});});
const sendBatch=(ops)=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});send('batch',{id,ops});});
addEventListener('message',(event)=>{
  const m=event.data;
  if(!m||m.channel!==CHANNEL||m.nonce!==NONCE)return;
  if(m.type==='rpc-result'){
    __kdMergeMirror(m.mirror);
    const p=pending.get(m.id);if(!p)return;pending.delete(m.id);
    m.ok?p.resolve(m.value):p.reject(new Error(String(m.error||'RPC 失败')));
  } else if(m.type==='geometry'){
    // 宿主容器几何镜像推送(聊天滚动/宿主窗口 resize 时节流推送)
    __kdSetGeo(m.geo);
  } else if(m.type==='jq-event'){
    __kdMergeMirror(m.mirror);
    __kdMergeEventTarget(m.target);
    // 事件回调异常仅告警不拆沙箱:作者脚本的单次点击报错(如跨域访问 window.parent.document)
    // 不应导致整个状态栏被 cleanup 拆除(此前此处上送 error 会触发宿主 finish→界面死亡)
    const fn=jqHandlers.get(m.jqId);if(fn)Promise.resolve().then(()=>fn.call(__kdWrapTarget(m.target),m.event||{})).then(flush).catch(e=>send('warn',{message:'事件回调出错: '+(e&&e.message?e.message:e)}));
  } else if(m.type==='inline-event'){
    // inline 事件降级桥:宿主把元素 data-kd-on* 属性里的作者代码串转回沙箱求值。
    // 优先走 __kdEvalInline(消息级单段由 IIFE 内注册,直接 eval——作用域链含作者脚本的
    // function 声明与 const 绑定;否则 onclick="submitCreation()" 这类调用在全局域
    // ReferenceError,这正是赛马娘卡首楼输入框/弹窗/复制按钮全部失效的主因)。
    // 无该桥(卡级 realm:作者脚本按 <script> 注入即全局作用域)时回退 new Function。
    __kdMergeMirror(m.mirror);
    __kdMergeEventTarget(m.target);
    try{
      const wrapped=__kdWrapTarget(m.target);
      const ev=Object.assign({preventDefault:function(){},stopPropagation:function(){},target:wrapped},m.event||{});
      const bridge=globalThis.__kdEvalInline;
      let run;
      if(typeof bridge==='function'){
        run=()=>bridge.call(wrapped,String(m.code||''),ev);
      }else{
        // 无闭包桥(卡级 realm:作者脚本即全局作用域)时回退 new Function;
        // 构造保持同步,语法错误仍走「解析失败」告警分支(与旧行为一致)
        const handler=new Function('event',String(m.code||''));
        run=()=>handler.call(wrapped,ev);
      }
      Promise.resolve().then(run).then(flush).catch(e=>send('warn',{message:'内联事件回调出错: '+(e&&e.message?e.message:e)}));
    }catch(e){send('warn',{message:'内联事件代码解析失败: '+(e&&e.message?e.message:e)});}
  } else if(m.type==='mvu-event'){
    // 宿主变量广播:合并最新变量树并触发本地事件订阅(状态栏的 MVU 更新监听)。
    // 广播可能在 boot 脚本求值前到达(ready 后宿主即注册 liveSandboxes),故整体容错。
    try{
      if(m.variables&&typeof m.variables==='object'){
        variables.stat_data=m.variables.stat_data&&typeof m.variables.stat_data==='object'?m.variables.stat_data:{};
        variables.display_data=m.variables.display_data&&typeof m.variables.display_data==='object'?m.variables.display_data:variables.stat_data;
      }
      __kdFireEvent(m.event,m.payload);
    }catch(_e){/* boot 前到达的广播丢弃 */}
  } else if(m.type==='card-event'){
    // 宿主卡级事件广播(swipe 切换等):直接派发到事件总线,供 eventOn 订阅
    // (舰娘卡「swipe 切换自动开关世界书」脚本监听 tavern_events.MESSAGE_SWIPED)
    try{__kdFireEvent(m.name,m.payload);}catch(_e){/* boot 前到达的广播丢弃 */}
  } else if(m.type==='shared-update'){
    // 宿主共享全局广播(同角色其余沙箱 publish 的全量快照):合法键赋到 window
    // 并并入基线防回声(自己注入的键不再 diff 上报回去)
    try{__kdSharedApply(m.globals);}catch(_e){/* boot 前到达的广播丢弃(TDZ) */}
  }
});
const warn=(m)=>send('warn',{message:String(m)});
// 事件 this 门面(jq-event 与 inline-event 共用):保留 kind/id/data/state 供 $() 再包装,
// 同时补齐 DOM 常用直读属性——作者代码里的 this.classList.contains('ready')/this.value/
// this.checked/this.dataset 等不再拿到 undefined。
const __kdWrapTarget=(t)=>{
  const base=t&&typeof t==='object'?t:{};
  const st=base.state&&typeof base.state==='object'?base.state:{};
  const classes=Array.isArray(st.classes)?st.classes:[];
  const data=base.data&&typeof base.data==='object'?base.data:{};
  return Object.assign({},base,{
    value:typeof st.val==='string'?st.val:'',
    checked:!!st.checked,
    disabled:!!st.disabled,
    dataset:data,
    classList:{contains:function(c){return classes.indexOf(String(c))>=0;}},
    textContent:typeof st.text==='string'?st.text:'',
    scrollTop:typeof st.scrollTop==='number'?st.scrollTop:0,
    scrollLeft:typeof st.scrollLeft==='number'?st.scrollLeft:0,
    scrollHeight:typeof st.scrollHeight==='number'?st.scrollHeight:0,
    scrollWidth:typeof st.scrollWidth==='number'?st.scrollWidth:0,
    clientHeight:typeof st.clientHeight==='number'?st.clientHeight:0,
    clientWidth:typeof st.clientWidth==='number'?st.clientWidth:0,
    offsetHeight:typeof st.offsetHeight==='number'?st.offsetHeight:0,
    offsetWidth:typeof st.offsetWidth==='number'?st.offsetWidth:0,
    getAttribute:function(k){const n=String(k);
      if(n==='id')return typeof st.id==='string'?st.id:null;
      if(n==='name')return typeof st.name==='string'?st.name:null;
      if(n==='value')return typeof st.val==='string'?st.val:null;
      if(n.indexOf('data-')===0){const key=n.slice(5).replace(/-([a-z])/g,function(_,c){return c.toUpperCase();});return key in data?String(data[key]):null;}
      return null;},
  });
};
// ---- 变量路径工具(防原型污染,与宿主 mvu/variables 一致) ----
const FORBIDDEN=new Set(['__proto__','constructor','prototype']);
const pathGet=(obj,path)=>{if(path==='')return obj;const segs=path.split('.');if(segs.some(s=>FORBIDDEN.has(s)))return undefined;let cur=obj;for(const seg of segs){if(cur===null||cur===undefined)return undefined;if(typeof cur!=='object')return undefined;cur=cur[seg];}return cur;};
const pathSet=(obj,path,value)=>{if(path===''){if(typeof value==='object'&&value!==null)Object.assign(obj,value);return;}const segs=path.split('.');if(segs.some(s=>FORBIDDEN.has(s)))return;let cur=obj;for(let i=0;i<segs.length-1;i++){const seg=segs[i];const next=cur[seg];const isIndex=/^\\d+$/.test(segs[i+1]);if(typeof next!=='object'||next===null){const created=isIndex?[]:{};cur[seg]=created;cur=created;}else{cur=next;}}cur[segs[segs.length-1]]=value;};
// ---- lodash 子集(stat_data 叶子为 [新值,原因],_.get 自动取 [0]) ----
// 镜像:此处 _.get 的解包段(Array.isArray(v)&&v.length>=1 → v[0])与
// mvu/unwrap.ts 的 unwrapStatLeaf 语义同步,改动必须双侧同改。
const _=Object.freeze({get:(obj,path,def)=>{let v=pathGet(obj,path);if(Array.isArray(v)&&v.length>=1)v=v[0];return v===undefined||v===null?def:v;},set:(obj,path,value)=>{pathSet(obj,path,value);return obj;}});
// ---- 兼容 API ----
const waitGlobalInitialized=()=>Promise.resolve();
const errorCatched=(fn)=>(...args)=>{try{return fn.apply(this,args);}catch(e){warn('脚本回调出错: '+(e&&e.message?e.message:e));}};
// ---- window.parent 门面:沙箱是不透明来源,跨源读父窗口属性(作者脚本常以
// window.parent 当酒馆主窗口:ST_WIN=window.parent 后 ST_WIN.Mvu / ST_WIN.waitGlobalInitialized)
// 会抛 SecurityError,环境检测类逻辑整段 reject 卡死。门面:postMessage 转发真父窗口,
// 其余属性读一律落到沙箱自身 globalThis(Mvu/waitGlobalInitialized/toastr 已挂载,
// 读不到返回 undefined——作者脚本的「检测」语义即判 undefined,正常降级为「未检测到」)。
const __kdRealParent=parent;
const __kdParentFacade=new Proxy({},{
  get:function(_t,k){
    if(k==='postMessage')return function(){return __kdRealParent.postMessage.apply(__kdRealParent,arguments);};
    // 父视口尺寸:作者定位代码(parentWin.innerHeight)要的是宿主窗口,不是容器高度
    if(k==='innerHeight')return __kdGeo.hostH;
    if(k==='innerWidth')return __kdGeo.hostW;
    return globalThis[k];
  },
});
try{
  Object.defineProperty(window,'parent',{configurable:true,enumerable:true,get:function(){return __kdParentFacade;}});
}catch(e){warn('parent 门面安装失败(不影响主流程): '+(e&&e.message?e.message:e));}
// ---- 宿主几何镜像:作者定位代码(wuwa positionOverlay 等)在酒馆里读 iframe 视口高、
//      window.frameElement 位置、document.body 矩形来算可见区中点;本沙箱是 display:none 的
//      隐藏框架,这些值全为 0,悬浮层会漂到视口外。这里把它们统一映射到宿主容器几何,
//      由宿主在 boot 与滚动/resize 时推送(type:'geometry')。
const __kdGeo={top:0,left:0,width:1280,height:720,scrollHeight:720,scrollWidth:1280,hostH:720,hostW:1280};
const __kdSetGeo=function(g){if(!g||typeof g!=='object')return;for(const k in __kdGeo){const v=Number(g[k]);if(Number.isFinite(v))__kdGeo[k]=v;}};
__kdSetGeo(JSON.parse(${JSON.stringify(initialGeo)})||undefined);
try{Object.defineProperty(window,'innerHeight',{configurable:true,get:function(){return __kdGeo.height;}});}catch(_e){}
try{Object.defineProperty(window,'innerWidth',{configurable:true,get:function(){return __kdGeo.width;}});}catch(_e){}
// frameElement:返回宿主容器在宿主视口中的实时矩形(作者靠它感知聊天滚动)
try{Object.defineProperty(window,'frameElement',{configurable:true,get:function(){return {getBoundingClientRect:function(){return {top:__kdGeo.top,left:__kdGeo.left,right:__kdGeo.left+__kdGeo.width,bottom:__kdGeo.top+__kdGeo.height,width:__kdGeo.width,height:__kdGeo.height,x:__kdGeo.left,y:__kdGeo.top};}};}});}catch(_e){}
// document.body/documentElement 矩形 = 容器内容盒(容器自身不滚动,故顶/左恒为 0)
const __kdBodyRect=function(){return {top:0,left:0,right:__kdGeo.width,bottom:__kdGeo.height,width:__kdGeo.width,height:__kdGeo.height,x:0,y:0};};
try{document.body.getBoundingClientRect=__kdBodyRect;Object.defineProperty(document.body,'scrollHeight',{configurable:true,get:function(){return __kdGeo.scrollHeight;}});Object.defineProperty(document.body,'scrollWidth',{configurable:true,get:function(){return __kdGeo.scrollWidth;}});}catch(_e){}
try{if(document.documentElement){document.documentElement.getBoundingClientRect=__kdBodyRect;Object.defineProperty(document.documentElement,'scrollHeight',{configurable:true,get:function(){return __kdGeo.scrollHeight;}});Object.defineProperty(document.documentElement,'scrollWidth',{configurable:true,get:function(){return __kdGeo.scrollWidth;}});}}catch(_e){}
// ---- localStorage 持久化桥(对齐酒馆:源级存储,重载/重渲染后仍在;如 wuwa 协议只需同意一次)。
// sessionStorage 保持会话级内存存储。两个实例必须独立(此前共用一个对象,协议状态互相污染)。
const __kdMakeStorage=function(saved,persist){
  const store=Object.assign({},saved||{});
  const sync=function(op,k,v){if(persist){try{send('local-storage',{op:op,key:k,value:v});}catch(_e){}}};
  return{getItem: function(k){return Object.prototype.hasOwnProperty.call(store,k)?store[k]:null;},setItem: function(k,v){store[k]=String(v);sync('set',k,store[k]);},removeItem: function(k){delete store[k];sync('remove',k);},clear: function(){for(const k in store)delete store[k];sync('clear');},key: function(i){return Object.keys(store)[i]||null;},get length(){return Object.keys(store).length;}};
};
const __kdMemoryStorage=__kdMakeStorage(__kdSavedLocal,true);
const __kdSessionStorage=__kdMakeStorage(null,false);
// ---- toastr stub:作者脚本经 window.parent.toastr 桥接失败(宿主无 toastr)时仍可调用 ----
const toastr=Object.freeze({info:function(m){console.info('[kedai-toastr]',m);},success:function(m){console.info('[kedai-toastr]',m);},warning:function(m){console.warn('[kedai-toastr]',m);},error:function(m){console.warn('[kedai-toastr]',m);},remove:function(){},clear:function(){},options:{}});
// ---- jQuery 子集(同步 API 外观;操作入队,脚本同步段结束后批量 RPC 提交,
//      微任务 flush 保证 getter/setter 链式调用在脚本 await 点前全部发出) ----
// getter 真实化(宿主状态镜像):宿主在每批操作与每个事件回包中附带容器状态
// (批内涉及的选择器状态 + 全部表单控件值),合并进 __kdMirror;getter 从镜像读,
// 使开场协议勾选/确认($('#agree').prop('checked') 等)读到真实值而非假值桩。
// 未命中的选择器 getter 会顺带 enqueue 一次 probe,下一批回包后同选择器即可读到真值。
const jqHandlers=new Map();
// jQuery 事件名规整:剥离命名空间('click.myNS' → 'click')并按空格拆多事件名。
// 旧实现把整串当事件名 addEventListener,带命名空间的绑定永不触发(实跑问题 7 R3)。
const __kdEventNames=(evt)=>String(evt??'').split(/\\s+/).map(function(s){var i=s.indexOf('.');return i<0?s:s.slice(0,i);}).filter(Boolean);
// jQuery on 参数重载:on(evt, fn) 直接绑定;on(evt, selector, fn) 委托绑定
// (动态弹窗/输入框最依赖委托;旧实现把 selector 字符串当回调存入 jqHandlers,
// 事件到来时 fn.call 抛 TypeError 被吞掉,表现即「绑定静默失效」——实跑问题 7 R3)。
// 注意:必须用 function 声明(不能用箭头函数)——下方依赖 arguments 读重载参数。
const __kdParseOn=function(a,b,c){
  if(typeof b==='function')return{evt:a,selector:null,fn:b};
  if(typeof c==='function')return{evt:a,selector:typeof b==='string'&&b?b:null,fn:c};
  return null;
};
// 已应用 draggable 的选择器键集合:data('ui-draggable') 与 destroy 判定用
const jqDraggableApplied={};
const jqReadyQueue=[];
let jqIdSeq=0;
let ops=[];
let flushPromise=null;
const __kdMirror={selectors:JSON.parse(${JSON.stringify(initialMirror)}),controls:JSON.parse(${JSON.stringify(initialControls)})};
// id 反向索引:attr('id') 读复合类选择器('.tab-page.active')时回查。
// jQuery 语义 = 集合首元素的 id;镜像只按「原样选择器串」采集,复合选择器可能从未入镜像,
// 但同族简单选择器('.tab-page' 的 items)与写操作补丁('#page-user' addClass)已在索引里。
const __kdById={};
const __kdIndexState=function(st){if(st&&typeof st.id==='string'&&st.id)__kdById[st.id]=st;};
// 表单控件并入 #id 选择器镜像:controls 携带 val/checked/disabled/selectedIndex/options,
// 而 id 轻量表(initialMirror)只有 classes——门面与 jQuery 子集的 read() 优先读
// __kdMirror.selectors['#id'],不并入就读不到控件真值(实测踩坑)。
const __kdMergeControls=function(list){
  if(!Array.isArray(list))return;
  __kdMirror.controls=list;
  for(const c of list){
    if(!c||typeof c.id!=='string'||!c.id)continue;
    const k='#'+c.id;
    const merged=Object.assign({n:1},__kdMirror.selectors[k]||{},c);
    __kdMirror.selectors[k]=merged;
    __kdIndexState(merged);
  }
};
// boot 注入的 id 轻量表立即入索引(boot 内同步执行的 render 读 attr('id') 时无需等 ack)
for(const __k in __kdMirror.selectors)__kdIndexState(__kdMirror.selectors[__k]);
__kdMergeControls(__kdMirror.controls);
const __kdMergeMirror=function(m){
  if(!m||typeof m!=='object')return;
  const S=m.selectors||{};
  for(const k in S){
    __kdMirror.selectors[k]=S[k];
    __kdIndexState(S[k]);
    if(S[k]&&Array.isArray(S[k].items))S[k].items.forEach(__kdIndexState);
  }
  if(Array.isArray(m.controls))__kdMergeControls(m.controls);
};
// 复合类选择器回查:'.tab-page.active' / 'div.a.b' → 第一个 classes 全含的元素 id
const __kdByClassLookup=function(sel){
  const m=/^[a-zA-Z][\\w-]*((?:\\.[\\w-]+)+)$/.exec(sel)||/^((?:\\.[\\w-]+)+)$/.exec(sel);
  if(!m)return null;
  const cls=(m[1]||'').split('.').filter(Boolean);
  if(cls.length===0)return null;
  // 优先同源前缀选择器的 items('.tab-page.active' → '.tab-page' 采集快照)
  if(cls.length>1){
    const prefix='.'+cls.slice(0,-1).join('.');
    const pe=__kdMirror.selectors[prefix];
    if(pe&&Array.isArray(pe.items)){
      for(const it of pe.items){
        if(it&&Array.isArray(it.classes)&&it.classes.indexOf(cls[cls.length-1])>=0&&typeof it.id==='string'&&it.id)return it.id;
      }
    }
  }
  for(const id in __kdById){
    const st=__kdById[id];
    if(!st||!Array.isArray(st.classes))continue;
    let ok=true;
    for(const c of cls)if(st.classes.indexOf(c)<0){ok=false;break;}
    if(ok)return id;
  }
  return null;
};
// 事件回包里的 target.state 是事件时刻的真值(滚动度量/勾选态等):按元素 id 合入选择器镜像,
// 事件回调里重新 $() 查询(如 $card[0].scrollTop)读到的是当前值而不是 boot 时的陈值。
const __kdMergeEventTarget=function(t){
  if(!t||typeof t!=='object')return;
  const st=t.state;
  if(!st||typeof st!=='object'||typeof st.id!=='string'||!st.id)return;
  const k='#'+st.id;
  const cur=__kdMirror.selectors[k]||{};
  if(typeof cur.n!=='number')cur.n=1;
  __kdMirror.selectors[k]=Object.assign(cur,st);
};
const __kdFindControl=function(sel){
  if(typeof sel!=='string')return null;
  const C=__kdMirror.controls||[];
  if(sel.charAt(0)==='#'){
    const id=sel.slice(1);
    for(let i=0;i<C.length;i++){const c=C[i];if(c&&c.id===id)return c;}
  } else if(sel.indexOf('[name=')===0){
    const nm=sel.slice(6,sel.length-1).replace(/^["']|["']$/g,'');
    for(let i=0;i<C.length;i++){const c=C[i];if(c&&c.name===nm)return c;}
  }
  return null;
};
const __kdSelState=function(sel){return typeof sel==='string'?(__kdMirror.selectors[sel]||null):null;};
// setter 乐观回写:同一同步段内 set 后立刻 get 读到新值(与真 jQuery 一致)
const __kdPatch=function(sel,patch){
  if(typeof sel!=='string')return;
  const e=__kdMirror.selectors[sel]||(__kdMirror.selectors[sel]={});
  for(const k in patch)e[k]=patch[k];
  // 纯 #id 写操作:条目无 id 时从选择器补上,并进反向索引(attr('id') 回查链)
  if(!e.id&&/^#[\\w-]+$/.test(sel))e.id=sel.slice(1);
  __kdIndexState(e);
  const c=__kdFindControl(sel);
  if(c)for(const k in patch)c[k]=patch[k];
};
const __kdProbeQueue={};
const flush=async()=>{
  if(flushPromise)return flushPromise;
  flushPromise=(async()=>{
    while(ops.length){const batch=ops;ops=[];await sendBatch(batch);}
    while(jqReadyQueue.length){
      const q=jqReadyQueue.splice(0);
      for(const fn of q)await Promise.resolve(fn());
      while(ops.length){const batch=ops;ops=[];await sendBatch(batch);}
    }
  })().finally(()=>{flushPromise=null;});
  return flushPromise;
};
const enqueue=(op)=>{ops.push(op);queueMicrotask(()=>{void flush().catch(e=>warn('DOM 操作失败: '+(e&&e.message?e.message:e)));});};
const __kdRefKey=function(ref){return ref?(ref.kind==='selector'?ref.value:ref.kind==='selector-index'?ref.value+'@@'+ref.index:null):null;};
const probe=(ref)=>{const k=__kdRefKey(ref);if(k&&!__kdProbeQueue[k]){__kdProbeQueue[k]=1;enqueue({ref,method:'probe',args:[]});}};
// ---- document.getElementById 门面:沙箱是不透明来源,自身文档没有任何卡内元素,
//      作者脚本的裸 getElementById(...).value / .innerHTML / .style.display 全部落空
//      (实跑问题 7 R5:赛马娘卡首楼 select/input/按钮交互失效的第二道断点)。
//      宿主在 boot(initialMirror)与每次事件/批回包下发容器状态镜像——按 id 索引的
//      元素状态 + 全部表单控件;这里据此返回元素门面:读走镜像(同步真值,in 含 select
//      的 value/selectedIndex/options[i].text),写经既有 DOM 白名单 batch 回写宿主真实
//      元素(innerHTML/value/checked/disabled/style.*),并乐观补丁镜像保证同一同步段内
//      写后即读一致。未命中镜像时回退原生(沙箱空文档返回 null,交给作者代码的空值分支)。
const __kdReadElemState=function(id){
  const key='#'+id;
  return __kdMirror.selectors[key]||__kdById[id]||null;
};
const __kdElemFacade=function(entry){
  const id=String((entry&&entry.id)||'');
  const ref={kind:'selector',value:'#'+id};
  const read=function(){return __kdReadElemState(id)||entry||{};};
  const ctl=function(){const c=__kdFindControl('#'+id);return c||null;};
  const write=function(method,args,patch){if(id)enqueue({ref,method,args});if(id)__kdPatch(ref.value,patch);};
  const state=read();
  const fac={
    id:id,
    tagName:String(state.tag||'').toUpperCase(),
    getAttribute:function(k){const n=String(k);const s=read();
      if(n==='id')return id||null;
      if(n==='name')return typeof s.name==='string'?s.name:null;
      if(n==='value')return typeof s.val==='string'?s.val:null;
      if(n==='class')return Array.isArray(s.classes)?s.classes.join(' '):null;
      if(n.indexOf('data-')===0){const key=n.slice(5).replace(/-([a-z])/g,function(_,c){return c.toUpperCase();});return key in s?String(s[key]):null;}
      return null;},
    setAttribute:function(k,v){if(k==='value')write('val',[String(v??'')],{val:String(v??'')});},
    removeAttribute:function(){},
    hasAttribute:function(k){return String(k)==='id'?!!id:false;},
    classList:{
      contains:function(c){const s=read();return Array.isArray(s.classes)&&s.classes.indexOf(String(c))>=0;},
      add:function(){write('addClass',Array.prototype.slice.call(arguments),{});},
      remove:function(){write('removeClass',Array.prototype.slice.call(arguments),{});},
    },
    get className(){const s=read();return Array.isArray(s.classes)?s.classes.join(' '):'';},
    set className(v){write('attr',['class',String(v??'')],{});},
    dataset:Object.assign({},(entry&&entry.data)||{}),
    // 游离子树操作:本卡剪贴板兜底分支会 createElement + body.appendChild;
    // 沙箱不实现真子树,仅保证不抛错(主路径经 navigator.clipboard 桥,不走此处)。
    appendChild:function(c){return c;},
    removeChild:function(c){return c;},
    insertBefore:function(c){return c;},
    querySelector:function(){return null;},
    querySelectorAll:function(){return [];},
    getBoundingClientRect:function(){return {top:0,left:0,right:__kdGeo.width,bottom:__kdGeo.height,width:__kdGeo.width,height:__kdGeo.height,x:0,y:0};},
    closest:function(){return null;},
    remove:function(){},
    focus:function(){},
    blur:function(){},
    click:function(){if(id)enqueue({ref,method:'trigger',args:['click']});},
    contains:function(){return false;},
    get style(){return __kdElemStyleFacade(read,write);},
  };
  // 读属性:每次取镜像最新值(同一同步段内写后即读要走乐观补丁)
  for(const key of ['value','val']){
    Object.defineProperty(fac,key,{configurable:true,get:function(){const s=read();const c=ctl();if(c&&typeof c.val==='string')return c.val;return typeof s.val==='string'?s.val:'';},set:function(v){write('val',[String(v??'')],{val:String(v??'')});}});
  }
  Object.defineProperty(fac,'checked',{configurable:true,get:function(){const s=read();const c=ctl();return c?!!c.checked:!!s.checked;},set:function(v){write('prop',['checked',!!v],{checked:!!v});}});
  Object.defineProperty(fac,'disabled',{configurable:true,get:function(){const s=read();const c=ctl();return c?!!c.disabled:!!s.disabled;},set:function(v){write('prop',['disabled',!!v],{disabled:!!v});}});
  Object.defineProperty(fac,'selectedIndex',{configurable:true,get:function(){const c=ctl();const s=read();if(c&&typeof c.selectedIndex==='number')return c.selectedIndex;return typeof s.selectedIndex==='number'?s.selectedIndex:-1;}});
  Object.defineProperty(fac,'options',{configurable:true,get:function(){const c=ctl();const s=read();const src=c&&Array.isArray(c.options)?c.options:(Array.isArray(s.options)?s.options:[]);return src.map(function(o){return {value:String((o&&o.value)||''),text:String((o&&o.text)||'')};});}});
  Object.defineProperty(fac,'innerHTML',{configurable:true,get:function(){const s=read();return typeof s.html==='string'?s.html:'';},set:function(v){write('html',[String(v??'')],{html:String(v??'')});}});
  Object.defineProperty(fac,'outerHTML',{configurable:true,get:function(){const s=read();return typeof s.html==='string'?s.html:'';}});
  Object.defineProperty(fac,'textContent',{configurable:true,get:function(){const s=read();return typeof s.text==='string'?s.text:'';},set:function(v){write('text',[String(v??'')],{text:String(v??'')});}});
  Object.defineProperty(fac,'innerText',{configurable:true,get:function(){const s=read();return typeof s.text==='string'?s.text:'';},set:function(v){write('text',[String(v??'')],{text:String(v??'')});}});
  return fac;
};
const __kdElemStyleFacade=function(read,write){
  const cur=function(){const s=read();return (s&&s.css&&typeof s.css==='object')?s.css:{};};
  return new Proxy({},{
    get:function(_t,k){
      if(k==='setProperty')return function(prop,v){write('css',[String(prop),String(v)],{css:Object.assign({},cur(),{[String(prop)]:String(v)})});};
      if(k==='removeProperty')return function(prop){write('css',[String(prop),''],{css:Object.assign({},cur(),{[String(prop)]:''})});};
      if(k==='getPropertyValue')return function(prop){const c=cur();return c[String(prop)]||c[String(prop).replace(/-([a-z])/g,function(_m,ch){return ch.toUpperCase();})]||'';};
      if(k==='length')return 0;
      if(k==='item')return function(){return '';};
      const c=cur();return c[k]!==undefined?c[k]:'';
    },
    set:function(_t,k,v){
      if(k==='cssText'){return true;}
      write('css',[String(k),String(v)],{css:Object.assign({},cur(),{[String(k)]:String(v)})});
      return true;
    },
  });
};
// 覆写 document.getElementById:命中宿主镜像返回门面,未命中回退原生(空文档 null)。
// 只拦截 getElementById —— 本卡需求面;querySelector 等保持原生,最小化回归风险。
try{
  const __kdNativeGetById=document.getElementById.bind(document);
  Object.defineProperty(document,'getElementById',{configurable:true,writable:true,value:function(id){
    const key=String(id);
    const st=__kdReadElemState(key);
    const c=__kdFindControl('#'+key);
    if(!st&&!c)return __kdNativeGetById(key);
    return __kdElemFacade(c||st);
  }});
}catch(e){warn('document 查询门面安装失败(作者脚本裸 getElementById 将拿不到卡内元素): '+(e&&e.message?e.message:e));}
// $('<div …>') 创建游离元素:on 绑定与子元素先记在 spec 里,append 进真实容器时由宿主一并落 DOM
function jqCreated(spec){
  if(!spec.attrs)spec.attrs={};
  if(!spec.css)spec.css={};
  const coll={};
  Object.defineProperty(coll,'__kdSpec',{get:function(){return spec;}});
  Object.defineProperty(coll,'length',{get:function(){return 1;}});
  coll.on=function(evt,fn){const p=__kdParseOn.apply(null,arguments);if(!p)return coll;const id=++jqIdSeq;jqHandlers.set(id,p.fn);spec.handlers.push({evt:String(p.evt),jqId:id,selector:p.selector||null});return coll;};
  coll.off=function(){return coll;};
  coll.append=function(child){if(child&&child.__kdSpec)spec.children.push(child.__kdSpec);return coll;};
  coll.addClass=function(c){spec.addClass=(spec.addClass?spec.addClass+' ':'')+String(c??'');return coll;};
  coll.removeClass=function(){return coll;};
  coll.toggleClass=function(){return coll;};
  coll.text=function(v){if(arguments.length===0)return '';spec.text=String(v??'');return coll;};
  coll.html=function(v){if(arguments.length===0)return spec.innerHtml||'';spec.innerHtml=String(v??'');return coll;};
  // attr 三种形态:两参写、一参对象合并、一参字符串 getter(游离元素无镜像,返回 '')
  coll.attr=function(k,v){
    if(arguments.length>=2){spec.attrs[String(k)]=String(v??'');return coll;}
    if(k&&typeof k==='object'){for(const key in k)spec.attrs[key]=String(k[key]);return coll;}
    return '';
  };
  coll.prop=function(){return arguments.length===1?false:coll;};
  // css 三种形态:两参写、一参对象合并、一参字符串读 spec(与真 jQuery 链式语义一致)
  coll.css=function(k,v){
    if(arguments.length>=2){spec.css[String(k)]=String(v??'');return coll;}
    if(k&&typeof k==='object'){for(const key in k)spec.css[key]=String(k[key]);return coll;}
    return spec.css[String(k)]??'';
  };
  coll.val=function(){return arguments.length===0?'':coll;};
  coll.data=function(name){return String(name)==='ui-draggable'?!!(spec.draggable&&spec.draggable!=='destroy'):'';};
  coll.hasClass=function(){return false;};
  coll.is=function(q){if(q===':visible')return true;if(q===':hidden')return false;return false;};
  coll.find=function(){return coll;};
  coll.closest=function(){return coll;};
  coll.first=function(){return coll;};
  coll.last=function(){return coll;};
  coll.eq=function(){return coll;};
  coll.each=function(cb){if(typeof cb==='function')cb.call(spec,0,spec);return coll;};
  coll.click=function(fn){if(typeof fn==='function')return coll.on('click',fn);return coll;};
  coll.trigger=function(){return coll;};
  coll.remove=function(){return coll;};
  coll.empty=function(){spec.children.length=0;spec.innerHtml='';return coll;};
  coll.show=function(){return coll;};
  coll.hide=function(){return coll;};
  coll.toggle=function(show){if(typeof show==='boolean')return show?coll.show():coll.hide();return coll;};
  coll.fadeIn=function(){return coll;};
  coll.fadeOut=function(){return coll;};
  coll.slideDown=function(){return coll;};
  coll.slideUp=function(){return coll;};
  coll.animate=function(){return coll;};
  coll.focus=function(){return coll;};
  coll.outerHeight=function(){return 0;};
  // 游离元素记录 draggable 选项('destroy' 记录为字符串),宿主 appendCreated 落 DOM 时绑指针拖拽
  coll.draggable=function(opts){
    if(typeof opts==='string'){spec.draggable=opts;return coll;}
    const o=opts&&typeof opts==='object'?opts:{};
    const wire={};
    for(const k of ['start','drag','stop']){if(typeof o[k]==='function'){const id=++jqIdSeq;jqHandlers.set(id,o[k]);wire[k]=id;}else wire[k]=0;}
    for(const k of ['handle','cancel','containment','cursor']){if(typeof o[k]==='string')wire[k]=o[k];}
    if(typeof o.distance==='number')wire.distance=o.distance;
    spec.draggable=wire;
    return coll;
  };
  return coll;
}
function jq(sel){
  if(typeof sel==='function'){jqReadyQueue.push(sel);return;}
  if(typeof sel==='string'&&/^\\s*</.test(sel))return jqCreated({kind:'created',html:sel,handlers:[],children:[]});
  if(sel&&sel.kind==='created')return jqCreated(sel);
  const ref=typeof sel==='string'?{kind:'selector',value:sel}
    // $(document)/$(window):作者脚本以窗口为事件源($(window).on('unload', …));
    // 放在 target/selector-index 判定之前(这两个是沙箱内真实对象,非引用句柄)。
    // typeof 守卫:部分 vm 测试 harness 未提供 document 全局,直接引用会抛 ReferenceError。
    :typeof document!=='undefined'&&sel===document?{kind:'document'}
    :typeof window!=='undefined'&&sel===window?{kind:'window'}
    :sel&&sel.kind==='target'?{kind:'target',id:sel.id}
    :sel&&sel.kind==='selector-index'?{kind:'selector-index',value:sel.value,index:sel.index}
    :null;
  const isSnap=!!(sel&&sel.kind==='snapshot');
  const localData=sel&&(sel.kind==='target'||isSnap)&&sel.data?sel.data:{};
  const localState=sel&&sel.kind==='target'&&sel.state?sel.state:(isSnap?sel.data:null);
  const refKey=__kdRefKey(ref);
  const readState=function(){return localState||(refKey?__kdMirror.selectors[refKey]||null:null);};
  const coll={};
  coll.text=function(v){if(arguments.length===0){const st=readState();if(!st)probe(ref);return st&&typeof st.text==='string'?st.text:'';}if(ref)enqueue({ref,method:'text',args:[v]});__kdPatch(sel,{text:String(v??'')});return coll;};
  coll.html=function(v){if(arguments.length===0){const st=readState();if(!st)probe(ref);return st&&typeof st.html==='string'?st.html:'';}if(ref)enqueue({ref,method:'html',args:[v]});__kdPatch(sel,{html:String(v??'')});return coll;};
  // css 四种形态:0 参返回 '';1 参字符串 getter(从镜像读真值,无镜像 probe 后返回 '');
  // 1 参对象逐键 setter;2 参 setter(现行为)。对象形式此前被当 getter 吞掉,
  // 悬浮球 .css({position:'fixed',zIndex:9999,…}) 全部丢失 → 无定位无层叠。
  coll.css=function(p,v){
    if(arguments.length===0)return '';
    if(arguments.length===1){
      if(p&&typeof p==='object'){
        for(const k in p){if(ref)enqueue({ref,method:'css',args:[k,p[k]]});}
        __kdPatch(sel,{css:Object.assign({},(readState()||{}).css||{},p)});
        return coll;
      }
      const st=readState();
      if(!st){probe(ref);return '';}
      const cs=st.css&&typeof st.css==='object'?st.css:null;
      return cs&&typeof cs[p]==='string'?cs[p]:(typeof st.style==='object'&&st.style&&typeof st.style[p]==='string'?st.style[p]:'');
    }
    if(ref)enqueue({ref,method:'css',args:[p,v]});
    __kdPatch(sel,{css:Object.assign({},(readState()||{}).css||{},{[String(p)]:String(v??'')})});
    return coll;
  };
  coll.addClass=function(c){if(ref)enqueue({ref,method:'addClass',args:[c]});const st=readState();const cl=st&&Array.isArray(st.classes)?st.classes.slice():[];for(const x of String(c??'').split(/\\s+/).filter(Boolean))if(cl.indexOf(x)<0)cl.push(x);__kdPatch(sel,{classes:cl});return coll;};
  coll.removeClass=function(c){if(ref)enqueue({ref,method:'removeClass',args:[c]});const st=readState();const rm=String(c??'').split(/\\s+/).filter(Boolean);const cl=(st&&Array.isArray(st.classes)?st.classes:[]).filter(x=>rm.indexOf(x)<0);__kdPatch(sel,{classes:cl});return coll;};
  coll.hasClass=function(name){const st=readState();if(!st)probe(ref);const cl=st&&Array.isArray(st.classes)?st.classes:[];return cl.indexOf(String(name))>=0;};
  // data:ui-draggable 反映本集合是否已应用过 draggable(销毁钩子据此决定是否 destroy);
  // 其余键沿用事件 target.data / 本地数据
  coll.data=function(name){const n=String(name);if(n==='ui-draggable')return !!(refKey&&jqDraggableApplied[refKey]);return localData[n]??'';};
  coll.val=function(v){if(arguments.length===0){const c=__kdFindControl(sel);if(c&&typeof c.val==='string')return c.val;const st=readState();if(st&&typeof st.val==='string')return st.val;if(!st)probe(ref);return '';}if(ref)enqueue({ref,method:'val',args:[v]});__kdPatch(sel,{val:String(v??'')});return coll;};
  coll.on=function(evt,fn){const p=__kdParseOn.apply(null,arguments);if(ref&&p){const id=++jqIdSeq;jqHandlers.set(id,p.fn);enqueue({ref,method:'on',args:[p.evt,id,p.selector]});}return coll;};
  coll.off=function(evt){if(ref)enqueue({ref,method:'off',args:[evt]});return coll;};
  coll.hide=function(){if(ref)enqueue({ref,method:'css',args:['display','none']});__kdPatch(sel,{css:Object.assign({},(readState()||{}).css||{},{display:'none'})});return coll;};
  coll.show=function(){if(ref)enqueue({ref,method:'css',args:['display','']});__kdPatch(sel,{css:Object.assign({},(readState()||{}).css||{},{display:''})});return coll;};
  // toggle:boolean 参走 show/hide,无参按 css display 现状翻转(镜像读当前值)
  coll.toggle=function(show){if(typeof show==='boolean')return show?coll.show():coll.hide();const cur=coll.css('display');return cur==='none'?coll.show():coll.hide();};
  // animate:仅 scrollTop/scrollLeft 有等价操作(直接置位,无动画);其余属性 no-op 返回 coll
  coll.animate=function(props,ms,cb){if(props&&typeof props==='object'){for(const k in props){if(k==='scrollTop'||k==='scrollLeft'){if(ref)enqueue({ref,method:'css',args:[k,props[k]]});}}}if(typeof ms==='function')Promise.resolve().then(function(){ms();});else if(typeof cb==='function')Promise.resolve().then(function(){cb();});return coll;};
  // 开场界面脚本依赖的动画/属性方法:映射为等价操作(无动画,同步语义与 jQuery 一致)
  coll.slideDown=function(){if(ref)enqueue({ref,method:'css',args:['display','']});return coll;};
  coll.slideUp=function(){if(ref)enqueue({ref,method:'css',args:['display','none']});return coll;};
  coll.fadeIn=function(){if(ref)enqueue({ref,method:'css',args:['display','']});return coll;};
  coll.fadeOut=function(){if(ref)enqueue({ref,method:'css',args:['display','none']});return coll;};
  coll.prop=function(name,value){if(arguments.length===1){const c=__kdFindControl(sel);const st=readState();if(!c&&!st)probe(ref);if(name==='checked')return c?!!c.checked:st?!!st.checked:false;if(name==='disabled')return c?!!c.disabled:st?!!st.disabled:false;if(name==='value')return c&&typeof c.val==='string'?c.val:st&&typeof st.val==='string'?st.val:'';return false;}if(ref)enqueue({ref,method:'prop',args:[name,value]});if(name==='checked')__kdPatch(sel,{checked:!!value});else if(name==='disabled')__kdPatch(sel,{disabled:!!value});else if(name==='value')__kdPatch(sel,{val:String(value??'')});return coll;};
  coll.attr=function(name,value){
    if(arguments.length===1){
      // 1 参对象:逐键 setter(attr({id:'x',class:'y'}))
      if(name&&typeof name==='object'){
        for(const k in name){if(ref)enqueue({ref,method:'attr',args:[k,name[k]]});}
        const patch={};for(const k in name){if(k==='id')patch.id=String(name[k]);else if(k==='class')patch.classes=String(name[k]).split(/\\s+/).filter(Boolean);}
        if(Object.keys(patch).length)__kdPatch(sel,patch);
        return coll;
      }
      const st=readState();
      if(name==='id'){
        if(st&&typeof st.id==='string'&&st.id)return st.id;
        // 镜像无此选择器:复合类选择器走反向索引回查(jQuery 语义=集合首元素 id)
        const hit=typeof sel==='string'?__kdByClassLookup(sel):null;
        if(hit)return hit;
        if(!st)probe(ref);
        return '';
      }
      if(name==='class')return st&&Array.isArray(st.classes)?st.classes.join(' '):'';
      if(!st)probe(ref);
      return '';
    }
    if(ref)enqueue({ref,method:'attr',args:[name,value]});
    if(name==='id')__kdPatch(sel,{id:String(value??'')});
    return coll;
  };
  coll.trigger=function(evt){if(ref)enqueue({ref,method:'trigger',args:[evt]});return coll;};
  // append 支持 $('<div>') 游离元素(spec 经 appendCreated 由宿主落 DOM 并绑定延迟事件)
  coll.append=function(html){if(html&&html.__kdSpec){if(ref)enqueue({ref,method:'appendCreated',args:[html.__kdSpec]});return coll;}if(ref)enqueue({ref,method:'append',args:[String(html??'')]});return coll;};
  coll.focus=function(){if(ref)enqueue({ref,method:'focus',args:[]});return coll;};
  coll.is=function(q){const c=__kdFindControl(sel);const st=readState();if(!c&&!st)probe(ref);if(q===':checked')return c?!!c.checked:st?!!st.checked:false;if(q===':disabled')return c?!!c.disabled:st?!!st.disabled:false;
    // :visible/:hidden 从镜像 css.display 判定(wuwa 面板 $panel.is(':hidden') 决定展开);
    // 镜像无 display 信息时按合理默认(:visible=false / :hidden=true),不抛错
    if(q===':visible'||q===':hidden'){const cs=st&&st.css&&typeof st.css==='object'?st.css:null;const disp=cs&&typeof cs.display==='string'?cs.display:null;const hidden=disp===null?true:disp==='none';return q===':visible'?!hidden:hidden;}
    return false;};
  coll.outerHeight=function(){const st=readState();if(st&&typeof st.offsetHeight==='number')return st.offsetHeight;if(!st)probe(ref);return 0;};
  coll.find=function(){return coll;};
  coll.closest=function(){return coll;};
  coll.remove=function(){if(ref)enqueue({ref,method:'remove',args:[]});return coll;};
  coll.empty=function(){if(ref)enqueue({ref,method:'empty',args:[]});return coll;};
  // ---- wuwa 状态栏实测缺口:toggleClass/first/last/eq/click/each/length/索引访问 ----
  coll.toggleClass=function(c,force){if(ref)enqueue({ref,method:'toggleClass',args:[String(c??''),force]});const st=readState();const cl=st&&Array.isArray(st.classes)?st.classes.slice():[];for(const x of String(c??'').split(/\\s+/).filter(Boolean)){const i=cl.indexOf(x);const want=typeof force==='boolean'?!!force:i<0;if(want&&i<0)cl.push(x);else if(!want&&i>=0)cl.splice(i,1);}if(typeof sel==='string')__kdPatch(sel,{classes:cl});return coll;};
  coll.first=function(){return jq(ref&&ref.kind==='selector'?{kind:'selector-index',value:ref.value,index:0}:ref?ref:sel);};
  coll.last=function(){return jq(ref&&ref.kind==='selector'?{kind:'selector-index',value:ref.value,index:-1}:ref?ref:sel);};
  coll.eq=function(i){const n=Number(i)||0;return jq(ref&&ref.kind==='selector'?{kind:'selector-index',value:ref.value,index:n}:ref?ref:sel);};
  coll.click=function(fn){if(typeof fn==='function')return coll.on('click',fn);if(ref)enqueue({ref,method:'trigger',args:['click']});return coll;};
  // each:从镜像 items 同步迭代(元素状态随批处理回包与事件回包到达;未命中时 probe 补采)
  coll.each=function(cb){if(typeof cb!=='function')return coll;const st=readState();const items=st&&Array.isArray(st.items)?st.items:null;
    if(items){for(let i=0;i<items.length;i++){const snap={kind:'snapshot',data:items[i]};if(cb.call(snap,i,snap)===false)break;}}
    else if(ref)probe(ref);
    return coll;};
  // jQuery .length 是属性:集合大小从镜像读(getter 语义与真 jQuery 一致,无镜像时 probe 后先回 0)
  Object.defineProperty(coll,'length',{get:function(){const st=readState();if(st&&typeof st.n==='number')return st.n;if(localState)return 1;if(ref)probe(ref);return 0;}});
  // 数字索引访问($('.nav-btn').first()[0] 等):返回可再被 $() 包装的 selector-index 引用,
  // 并带上镜像里的直读属性($card[0].scrollTop/scrollHeight/clientHeight/checked/value 等——
  // 协议卡 checkBottom 就靠这些度量判断滚动到底)。未命中时 probe 补采,下个事件 tick 可读。
  for(let __i=0;__i<4;__i++)(function(idx){Object.defineProperty(coll,idx,{get:function(){
    let r;
    if(ref&&ref.kind==='selector')r={kind:'selector-index',value:ref.value,index:idx};
    else if(ref&&ref.kind==='selector-index')r={kind:'selector-index',value:ref.value,index:ref.index+idx};
    else if(localState)return idx===0?sel:undefined;
    else return undefined;
    const st=__kdMirror.selectors[r.value+'@@'+r.index]||(r.index===0?__kdMirror.selectors[r.value]:null);
    if(st){
      const gk=['scrollTop','scrollLeft','scrollHeight','scrollWidth','clientHeight','clientWidth','offsetHeight','offsetWidth'];
      for(const k of gk){if(typeof st[k]==='number')r[k]=st[k];}
      if(typeof st.val==='string')r.value=st.val;
      if('checked' in st)r.checked=!!st.checked;
      if('disabled' in st)r.disabled=!!st.disabled;
      if(st.id)r.id=st.id;
      if(typeof st.text==='string')r.textContent=st.text;
    } else probe(r);
    return r;
  }});})(__i);
  // draggable:字符串 'destroy' 解绑;对象把 start/drag/stop 换成 jqId,其余可序列化字段原样
  // 入队。宿主在 pointermove/up 时以 jq-event 回发 event.ui.position(沙箱回调读它)。
  coll.draggable=function(opts){
    if(typeof opts==='string'){
      if(ref&&opts==='destroy'){enqueue({ref,method:'draggable',args:['destroy']});if(refKey)delete jqDraggableApplied[refKey];}
      return coll;
    }
    const o=opts&&typeof opts==='object'?opts:{};
    const wire={};
    for(const k of ['start','drag','stop']){if(typeof o[k]==='function'){const id=++jqIdSeq;jqHandlers.set(id,o[k]);wire[k]=id;}else wire[k]=0;}
    for(const k of ['handle','cancel','containment','cursor']){if(typeof o[k]==='string')wire[k]=o[k];}
    if(typeof o.distance==='number')wire.distance=o.distance;
    if(ref){enqueue({ref,method:'draggable',args:[wire]});if(refKey)jqDraggableApplied[refKey]=1;}
    return coll;
  };
  return coll;
}
const $=jq;
// ---- stat_data 裸值视图:kedai 存储叶子为 [值,原因] 成对数组,但 MVU 生态脚本
//      (wuwa 浪潮状态栏等)按裸值直接属性读(String(u.是否是漂泊者)==='true'、
//      u.性别||'男')。门面对外返回解包视图:叶子 [x,原因字符串] 取 x,嵌套递归;
//      裸值与真实数组(长度≠2 或第二元素非字符串)原样保留。_.get 对裸值兼容。 ----
// 镜像:__kdUnwrap 是 mvu/unwrap.ts 的 unwrapStatTree 的沙箱内联副本(字符串注入无法 import),
// 语义同步,改动必须双侧同改。源函数 SHA-256 短哈希:6ccd4bda787e(口径见 unwrap.ts,
// 两侧哈希不一致即漂移)。
const __kdUnwrap=function(v){
  if(Array.isArray(v)){
    if(v.length===2&&typeof v[1]==='string')return v[0];
    return v.map(__kdUnwrap);
  }
  if(v&&typeof v==='object'){const o={};for(const k of Object.keys(v))o[k]=__kdUnwrap(v[k]);return o;}
  return v;
};
const __kdStatView=()=>__kdUnwrap(structuredClone(variables.stat_data));
// ---- 事件总线(状态栏经 eventOn(Mvu.events.VARIABLE_UPDATE_ENDED,…) 订阅变量更新;
//      宿主在应用 UpdateVariable 后广播 mvu-event,此处合并变量树并触发订阅) ----
const __kdEventBus=new Map();
const __kdFireEvent=function(ev,payload){const set=__kdEventBus.get(ev);if(!set)return;set.forEach(function(cb){Promise.resolve().then(function(){cb(payload);}).catch(function(e){warn('事件回调出错: '+(e&&e.message?e.message:e));});});};
const eventOn=function(ev,cb){if(typeof cb!=='function')return function(){};if(!__kdEventBus.has(ev))__kdEventBus.set(ev,new Set());__kdEventBus.get(ev).add(cb);return function(){const s=__kdEventBus.get(ev);if(s)s.delete(cb);};};
const eventOff=function(ev,cb){const s=__kdEventBus.get(ev);if(s)s.delete(cb);};
const eventEmit=function(ev,payload){__kdFireEvent(ev,payload);return Promise.resolve(true);};
const Mvu=Object.freeze({
  getVariables:()=>({stat_data:__kdStatView(),display_data:structuredClone(variables.display_data)}),
  getMvuData:()=>({stat_data:__kdStatView(),display_data:structuredClone(variables.display_data)}),
  waitGlobalInitialized,
  replaceMvuData:(data)=>{if(!data||typeof data!=='object')return;const stat=data.stat_data&&typeof data.stat_data==='object'?data.stat_data:{};variables.stat_data=stat;variables.display_data=(data.display_data&&typeof data.display_data==='object')?data.display_data:stat;},
  parseMessage:(text)=>{try{const raw=String(text??'');let cleaned=raw;const m=/<UpdateVariable>[\\s\\S]*?<\\/UpdateVariable>/gi.exec(cleaned);if(m)cleaned=cleaned.slice(0,m.index)+cleaned.slice(m.index+m[0].length);return{cleaned,commands:[]};}catch{return{cleaned:String(text??''),commands:[]};}},
  isDuringExtraAnalysis:false,
  // 事件名常量(对齐 MagVarUpdate 生态)+ 订阅 API(wuwa 状态栏检查 window.eventOn && window.Mvu 后订阅)
  events:Object.freeze({VARIABLE_UPDATE_ENDED:'mag_variable_updated',VARIABLE_UPDATE_STARTED:'mag_variable_update_started'}),
  on:eventOn,
  off:eventOff,
  emit:eventEmit,
});
const getAllVariables=()=>({stat_data:__kdStatView(),display_data:structuredClone(variables.display_data)});
// ---- 酒馆变量 API(全局/消息两级):全局级持久化到宿主 localStorage(按角色),状态栏设置跨渲染保留 ----
const getVariables=function(opts){if(opts&&opts.type==='global')return structuredClone(__kdGlobals);return {stat_data:__kdStatView(),display_data:structuredClone(variables.display_data)};};
const replaceVariables=function(vars,opts){
  const isGlobal=!!(opts&&opts.type==='global');
  const t=isGlobal?__kdGlobals:variables;
  if(vars&&typeof vars==='object'){
    for(const k of Object.keys(t))delete t[k];
    for(const k of Object.keys(vars))t[k]=vars[k];
  }
  if(isGlobal)rpc('global-save',__kdGlobals).catch(function(){});
  return Promise.resolve();
};
const insertOrAssignVariables=function(vars,opts){
  const isGlobal=!!(opts&&opts.type==='global');
  const t=isGlobal?__kdGlobals:variables.stat_data;
  if(vars&&typeof vars==='object')for(const k of Object.keys(vars))t[k]=vars[k];
  if(isGlobal)rpc('global-save',__kdGlobals).catch(function(){});
  return Promise.resolve();
};
// 挂到 window:作者脚本大量检查 window.eventOn / window.Mvu / typeof getVariables(const 不产生 window 属性)
globalThis.$=$;globalThis.jQuery=$;globalThis._=_;
globalThis.Mvu=Mvu;globalThis.getAllVariables=getAllVariables;
globalThis.getVariables=getVariables;globalThis.replaceVariables=replaceVariables;globalThis.insertOrAssignVariables=insertOrAssignVariables;
globalThis.eventOn=eventOn;globalThis.eventOff=eventOff;globalThis.eventEmit=eventEmit;
globalThis.waitGlobalInitialized=waitGlobalInitialized;globalThis.errorCatched=errorCatched;globalThis.toastr=toastr;
const Dom=Object.freeze({setText:(selector,text)=>rpc('setText',selector,text),setHtml:(selector,html)=>rpc('setHtml',selector,html),setAttribute:(selector,name,value)=>rpc('setAttribute',selector,name,value)});
const fetch=undefined,XMLHttpRequest=undefined,WebSocket=undefined,EventSource=undefined,indexedDB=undefined,caches=undefined,localStorage=__kdMemoryStorage,sessionStorage=__kdSessionStorage;
// ---- 酒馆事件常量(对齐酒馆助手 tavern_events;值即事件总线键) ----
// 广播源现状:MESSAGE_SWIPED 由宿主 swipeMessage 注入;其余常量列出供脚本引用,
// kedai 侧暂无对应语义的广播源不会触发(安全 no-op)。
const tavern_events=Object.freeze({
  MESSAGE_SWIPED:'message_swiped',
  MESSAGE_SENT:'message_sent',
  MESSAGE_RECEIVED:'message_received',
  MESSAGE_EDITED:'message_edited',
  MESSAGE_DELETED:'message_deleted',
  GENERATION_STARTED:'generation_started',
  GENERATION_ENDED:'generation_ended',
  CHAT_CHANGED:'chat_changed',
});
// ---- TavernHelper 音频 API(阶段五 5a):8 个方法对齐酒馆 @types/function/audio.d.ts ----
// 沙箱 CSP 断掉媒体加载(media-src 'none'),播放必须经宿主 <audio> 元素;故 setter 类
// (playAudio/pauseAudio/replaceAudioList/appendAudioList/setAudioSettings)fire-and-forget
// 发 'audio' 消息转宿主执行,getter 类从启动拉取的缓存读(就绪前回退默认,与 jQuery 子集
// hasClass 同步降级一致,不阻塞脚本)。启动时 rpc 拉取一次全量音频状态填缓存。
const __kdAudioDefaults={bgm:{enabled:true,mode:'repeat_all',muted:false,volume:50},ambient:{enabled:true,mode:'play_one_and_stop',muted:false,volume:50}};
const __kdAudioEmpty=function(){return {src:'',title:'',playing:false,progress:0};};
const __kdAudioCache={list:{bgm:[],ambient:[]},settings:{bgm:__kdAudioDefaults.bgm,ambient:__kdAudioDefaults.ambient},current:{bgm:__kdAudioEmpty(),ambient:__kdAudioEmpty()}};
rpc('audio-snapshot').then(function(v){
  if(!v||typeof v!=='object')return;
  const L=v.list||{},S=v.settings||{},C=v.current||{};
  __kdAudioCache.list={bgm:Array.isArray(L.bgm)?L.bgm:[],ambient:Array.isArray(L.ambient)?L.ambient:[]};
  __kdAudioCache.settings={
    bgm:S.bgm&&typeof S.bgm==='object'?Object.assign({},__kdAudioDefaults.bgm,S.bgm):__kdAudioDefaults.bgm,
    ambient:S.ambient&&typeof S.ambient==='object'?Object.assign({},__kdAudioDefaults.ambient,S.ambient):__kdAudioDefaults.ambient
  };
  __kdAudioCache.current={
    bgm:C.bgm&&typeof C.bgm==='object'?C.bgm:__kdAudioEmpty(),
    ambient:C.ambient&&typeof C.ambient==='object'?C.ambient:__kdAudioEmpty()
  };
}).catch(function(){});
const __kdAudioTitle=function(url){try{return decodeURIComponent(new URL(url).pathname.split('/').pop()||'')||url;}catch(e){return url;}};
const TavernHelper=Object.freeze({
  playAudio:function(type,audio){send('audio',{op:'playAudio',args:[type,audio&&typeof audio==='object'?audio:null]});},
  pauseAudio:function(type){send('audio',{op:'pauseAudio',args:[type]});},
  getAudioList:function(type){const t=type==='ambient'?'ambient':'bgm';return (__kdAudioCache.list[t]||[]).map(function(x){return {title:(x&&x.title)||'',url:(x&&x.url)||''};});},
  replaceAudioList:function(type,audioList){send('audio',{op:'replaceAudioList',args:[type,Array.isArray(audioList)?audioList:[]]});},
  appendAudioList:function(type,audioList){send('audio',{op:'appendAudioList',args:[type,Array.isArray(audioList)?audioList:[]]});},
  getAudioSettings:function(type){const t=type==='ambient'?'ambient':'bgm';return Object.assign({},__kdAudioDefaults[t]||__kdAudioDefaults[t]);},
  setAudioSettings:function(type,settings){send('audio',{op:'setAudioSettings',args:[type,settings&&typeof settings==='object'?settings:{}]});},
  getCurrentAudio:function(type){const t=type==='ambient'?'ambient':'bgm';const c=__kdAudioCache.current[t]||__kdAudioEmpty();return {src:c.src||'',title:c.title||'',playing:!!c.playing,progress:typeof c.progress==='number'?c.progress:0};},
  // ---- 事件与工具(卡级脚本依赖面:舰娘卡 swipe 世界书开关联动) ----
  errorCatched:errorCatched,
  eventOn:eventOn,
  eventOff:eventOff,
  eventEmit:eventEmit,
  // ---- 世界书(经宿主 RPC 读/写角色内嵌 character_book;写仅放行 uid+enabled 白名单) ----
  // getCurrentCharPrimaryLorebook 是同步接口:主世界书名随 boot 下发(__KD_LOREBOOK__),
  // 与 kedai 资源帧 shim 的 getCharWorldbookNames 同源(角色卡内嵌世界书即主世界书)
  getCurrentCharPrimaryLorebook:function(){return __KD_LOREBOOK__;},
  getCharWorldbookNames:function(){return __KD_LOREBOOK__?{primary:__KD_LOREBOOK__,additional:[]}:null;},
  getLorebookEntries:function(name){if(!__KD_LOREBOOK__||String(name)!==__KD_LOREBOOK__)return Promise.resolve(null);return rpc('lorebook-entries',String(name));},
  getWorldbook:function(name){return this.getLorebookEntries(name);},
  setLorebookEntries:function(name,list){if(!__KD_LOREBOOK__||String(name)!==__KD_LOREBOOK__)return Promise.resolve(false);return rpc('lorebook-set',String(name),Array.isArray(list)?list:[]);},
  updateWorldbookWith:function(name,fn){const self=this;return self.getLorebookEntries(name).then(function(entries){if(!entries||typeof fn!=='function')return entries;const next=fn(entries.map(function(e){return e;}));return Array.isArray(next)?self.setLorebookEntries(name,next).then(function(){return next;}):entries;});},
  // ---- 聊天消息读取(楼层序号 0-based;include_swipe 带 swipes/swipe_id) ----
  getChatMessages:function(id,opts){return rpc('chat-messages',Number(id)||0,opts&&typeof opts==='object'?opts:{});},
});
globalThis.TavernHelper=TavernHelper;
// ---- 剪贴板桥(实跑问题 7 R4)----
// 沙箱是不透明来源 + display:none 隐藏框架,无焦点/无用户激活,navigator.clipboard.writeText
// 与 document.execCommand('copy') 都不可能成功(权限与焦点双缺),「复制提示词」类按钮静默失效。
// 这里覆写 navigator.clipboard,把写入经 RPC 转给宿主页面执行(宿主页有真实用户手势与
// clipboard-write 权限);文本型写入放行,其余(读剪贴板等)不提供。
try{
  const __kdWriteItems=function(items){
    try{
      const arr=Array.isArray(items)?items:[];
      const jobs=[];
      for(const it of arr){
        // 仅支持纯文本 ClipboardItem(getType('text/plain') 返回 Blob/Promise)
        if(it&&typeof it.getType==='function'){
          jobs.push(Promise.resolve(it.getType('text/plain')).then(function(blob){
            return blob&&typeof blob.text==='function'?blob.text():String(blob??'');
          }));
        }
      }
      if(jobs.length===0)return Promise.resolve(true);
      return Promise.all(jobs).then(function(texts){
        return rpc('clipboard-write',texts.filter(Boolean).join('\\n'));
      });
    }catch(e){return Promise.resolve(false);}
  };
  const __kdClipboard={
    writeText:function(text){return rpc('clipboard-write',String(text??''));},
    write:__kdWriteItems,
  };
  Object.defineProperty(navigator,'clipboard',{configurable:true,enumerable:true,get:function(){return __kdClipboard;}});
}catch(e){warn('剪贴板桥安装失败(不影响主流程): '+(e&&e.message?e.message:e));}
// 降级路径:document.execCommand('copy') 转宿主剪贴板桥(旧卡脚本用)
try{
  const __kdExec=document.execCommand?document.execCommand.bind(document):null;
  document.execCommand=function(cmd){
    if(String(cmd)==='copy'){
      // 取当前选区文本(沙箱内选区);无选区时由调用方先 select(),此处尽力读取。
      // 返回值对齐原生语义:真的派发了文本才算成功。此前无条件 return true 会让
      // 作者脚本的 try 分支照常弹「已复制」,而剪贴板其实没有内容(与 clipboard-write
      // 桥同一类谎报问题);取不到文本时返回 false,落到作者脚本的 catch 分支。
      let text='';try{text=String(globalThis.getSelection?globalThis.getSelection():'')||'';}catch(_e){}
      if(!text)return false;
      void rpc('clipboard-write',text).catch(function(){});
      return true;
    }
    return __kdExec?__kdExec.apply(document,arguments):false;
  };
}catch(_e){}
// tavern_events 在 TavernHelper 同区定义(后于上方事件总线挂载区),此处挂载避开 TDZ
globalThis.tavern_events=tavern_events;
// ---- 跨 realm 共享全局桥(镜像:宿主 sandbox/shared-globals.ts 的键/值校验,语义同步,改动必须双侧同改) ----
// 卡级脚本把纯数据全局挂 window(如 wuwa MVU 卡 window.WuWaShared={STORY_MAP,...}),
// 其余 realm(消息级开场白沙箱)跨源直读不到;这里 diff window 自有可枚举键的
// 新增/引用变化上报宿主(shared-publish),宿主按角色持久化并广播(shared-update)。
const __kdSharedBadKeys=['window','document','top','parent','self','globalThis','frames','location','localStorage','sessionStorage','eval','Function','fetch','XMLHttpRequest','open','close'];
const __kdSharedKeyOk=function(k){return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(k)&&__kdSharedBadKeys.indexOf(k)<0&&k.indexOf('__kd')!==0;};
// 值粗筛:JSON 可序列化且序列化 ≤256KB(字符数口径,宿主侧按 UTF-8 字节精确复核)
const __kdSharedJson=function(v){try{const s=JSON.stringify(v);if(typeof s!=='string')return null;if(s.length>262144)return null;return s;}catch(_e){return null;}};
const __kdSharedBaseline=Object.create(null);
// 注入宿主下发快照(先于用户代码,脚本同步可读);注入键并进基线防回声
const __kdSharedInitial=JSON.parse(${JSON.stringify(initialShared)});
for(const __sk in __kdSharedInitial){if(__kdSharedKeyOk(__sk)){try{window[__sk]=__kdSharedInitial[__sk];}catch(_e){}}}
// 基线快照:用户代码运行前 window 自有可枚举键(boot 挂载的 $/Mvu 等收录,diff 不上报)
for(const __bk of Object.keys(window)){try{__kdSharedBaseline[__bk]=window[__bk];}catch(_e){}}
const __kdSharedApply=function(g){
  if(!g||typeof g!=='object')return;
  for(const k in g){if(!__kdSharedKeyOk(k))continue;try{window[k]=g[k];__kdSharedBaseline[k]=window[k];}catch(_e){}}
};
const __kdSharedDiff=function(){
  const out={};let changed=false;
  for(const k of Object.keys(window)){
    if(!__kdSharedKeyOk(k))continue;
    let v;try{v=window[k];}catch(_e){continue;}
    if(Object.prototype.hasOwnProperty.call(__kdSharedBaseline,k)&&__kdSharedBaseline[k]===v)continue;
    const s=__kdSharedJson(v);
    if(s===null)continue; // 函数/循环引用/超大值:跳过(宿主侧同规则再校验一道)
    out[k]=JSON.parse(s);
    try{__kdSharedBaseline[k]=window[k];}catch(_e){}
    changed=true;
  }
  if(changed)send('shared-publish',{globals:out});
};
let __kdSharedTimer=null;
// done 时立即 diff 一次(同步段挂的全局即时上报),之后每 2s 周期 diff(异步挂载兜底);
// 计时器无需显式 clear:沙箱 realm 随 iframe 销毁整体回收
const __kdSharedStart=function(){__kdSharedDiff();if(__kdSharedTimer===null&&typeof setInterval==='function')__kdSharedTimer=setInterval(__kdSharedDiff,2000);};
${runUserCode}
`;
}
