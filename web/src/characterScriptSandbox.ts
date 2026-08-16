import sanitizeHtml from 'sanitize-html';
import type { MvuVariables } from './mvu/variables';
import { getAudioController } from './audioController';
import type { AudioChannelSettings, AudioChannelType } from './api/types';

const CHANNEL = 'kedai-character-script-v1';
/** 沙箱文档需要一次网络往返(GET /sandbox.html)后才能 boot,超时留足余量 */
const DEFAULT_TIMEOUT_MS = 8_000;
/**
 * 沙箱 boot 消息上限。角色卡开场/状态栏脚本动辄几十 KB(如 WuWa 状态栏 107KB,
 * 开场 40KB),64KB 会把它们整段拒掉 → 协议弹窗/悬浮球/状态栏永不显示。
 * postMessage 跨窗口传输无浏览器级硬上限(与内存一致),这里放宽到 1MB 防内存压力。
 */
const MAX_MESSAGE_BYTES = 1024 * 1024;
/**
 * 沙箱 bootstrap 文档。必须由服务端下发(见 server-rs security::sandbox_headers),
 * 不能用 srcdoc:srcdoc iframe 继承父页面 CSP,而全站 CSP 是 script-src 'self'
 * (无 'unsafe-inline'),内联脚本会被浏览器直接拒绝,状态栏永远停在「加载中」。
 */
const SANDBOX_DOCUMENT_URL = '/sandbox.html';

interface JqOperation {
  ref: { kind: 'selector'; value: string } | { kind: 'target'; id: number };
  method: string;
  args: unknown[];
}

interface SandboxRequest {
  channel: typeof CHANNEL;
  nonce: string;
  type: 'ready' | 'rpc' | 'batch' | 'done' | 'error' | 'warn' | 'jq-event' | 'audio';
  id?: number;
  op?: string;
  args?: unknown[];
  ops?: JqOperation[];
  value?: unknown;
  message?: string;
  jqId?: number;
}

export interface SandboxExecutionContext {
  container: HTMLElement;
  variables: MvuVariables;
}

export interface SandboxEnvironment {
  document: Pick<Document, 'createElement' | 'body'>;
  window: Pick<Window, 'addEventListener' | 'removeEventListener'>;
  timeoutMs?: number;
}

export type SandboxCleanup = () => void;

export const MAX_BOOT_MESSAGE_BYTES = MAX_MESSAGE_BYTES;

function cloneData<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function utf8Bytes(value: unknown): number {
  return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}

function safeSelector(value: unknown): string {
  if (typeof value !== 'string' || value.length > 256 || /[\0\r\n]/.test(value)) {
    throw new Error('选择器无效');
  }
  return value;
}

function safeText(value: unknown): string {
  if (typeof value !== 'string' || value.length > 20_000) throw new Error('文本无效');
  return value;
}

export function sandboxAttributes(): Readonly<Record<string, string>> {
  return { sandbox: 'allow-scripts', referrerpolicy: 'no-referrer', 'aria-hidden': 'true' };
}

/**
 * 沙箱 iframe 内容。除基本变量/通信外,内联补全 MagVarUpdate 生态脚本依赖的兼容 API:
 *   - _ (lodash 子集 get/set,叶子 [新值,原因] 自动取 [0])
 *   - $ (jQuery 子集,经 RPC 转发到宿主容器操作 DOM)
 *   - waitGlobalInitialized / errorCatched
 *   - Mvu / getAllVariables / Dom
 * 沙箱是隔离窗口,看不到宿主 window 上的 installMvuGlobals,因此这里必须自带全部全局。
 */
export function sandboxScript(code: string, nonce: string, variables: MvuVariables): string {
  const escapedCode = code.replace(/<\/script/gi, '<\\/script');
  const initialData = JSON.stringify(cloneData(variables)).replace(/</g, '\\u003c');
  const initialNonce = JSON.stringify(nonce);
  return `'use strict';
const CHANNEL=${JSON.stringify(CHANNEL)}, NONCE=${initialNonce};
const variables=JSON.parse(${JSON.stringify(initialData)});
let seq=0;
const pending=new Map();
const send=(type, extra={})=>parent.postMessage({channel:CHANNEL,nonce:NONCE,type,...extra}, '*');
const rpc=(op,...args)=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});send('rpc',{id,op,args});});
const sendBatch=(ops)=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});send('batch',{id,ops});});
addEventListener('message',(event)=>{
  const m=event.data;
  if(!m||m.channel!==CHANNEL||m.nonce!==NONCE)return;
  if(m.type==='rpc-result'){
    const p=pending.get(m.id);if(!p)return;pending.delete(m.id);
    m.ok?p.resolve(m.value):p.reject(new Error(String(m.error||'RPC 失败')));
  } else if(m.type==='jq-event'){
    const fn=jqHandlers.get(m.jqId);if(fn)Promise.resolve().then(()=>fn.call(m.target,m.event||{})).then(flush).catch(e=>send('error',{message:'事件回调出错: '+(e&&e.message?e.message:e)}));
  }
});
const warn=(m)=>send('warn',{message:String(m)});
// ---- 变量路径工具(防原型污染,与宿主 mvu/variables 一致) ----
const FORBIDDEN=new Set(['__proto__','constructor','prototype']);
const pathGet=(obj,path)=>{if(path==='')return obj;const segs=path.split('.');if(segs.some(s=>FORBIDDEN.has(s)))return undefined;let cur=obj;for(const seg of segs){if(cur===null||cur===undefined)return undefined;if(typeof cur!=='object')return undefined;cur=cur[seg];}return cur;};
const pathSet=(obj,path,value)=>{if(path===''){if(typeof value==='object'&&value!==null)Object.assign(obj,value);return;}const segs=path.split('.');if(segs.some(s=>FORBIDDEN.has(s)))return;let cur=obj;for(let i=0;i<segs.length-1;i++){const seg=segs[i];const next=cur[seg];const isIndex=/^\\d+$/.test(segs[i+1]);if(typeof next!=='object'||next===null){const created=isIndex?[]:{};cur[seg]=created;cur=created;}else{cur=next;}}cur[segs[segs.length-1]]=value;};
// ---- lodash 子集(stat_data 叶子为 [新值,原因],_.get 自动取 [0]) ----
const _=Object.freeze({get:(obj,path,def)=>{let v=pathGet(obj,path);if(Array.isArray(v)&&v.length>=1)v=v[0];return v===undefined||v===null?def:v;},set:(obj,path,value)=>{pathSet(obj,path,value);return obj;}});
// ---- 兼容 API ----
const waitGlobalInitialized=()=>Promise.resolve();
const errorCatched=(fn)=>(...args)=>{try{return fn.apply(this,args);}catch(e){warn('脚本回调出错: '+(e&&e.message?e.message:e));}};
// ---- 内存存储 shim:作者脚本(如 WuWa 开场协议)读写 localStorage 不抛错,状态仅存内存 ----
const __kdMemoryStorage=(function(){const store={};return{getItem: function(k){return Object.prototype.hasOwnProperty.call(store,k)?store[k]:null;},setItem: function(k,v){store[k]=String(v);},removeItem: function(k){delete store[k];},clear: function(){for(const k in store)delete store[k];},key: function(i){return Object.keys(store)[i]||null;},get length(){return Object.keys(store).length;}};})();
// ---- toastr stub:作者脚本经 window.parent.toastr 桥接失败(宿主无 toastr)时仍可调用 ----
const toastr=Object.freeze({info:function(m){console.info('[kedai-toastr]',m);},success:function(m){console.info('[kedai-toastr]',m);},warning:function(m){console.warn('[kedai-toastr]',m);},error:function(m){console.warn('[kedai-toastr]',m);},remove:function(){},clear:function(){},options:{}});
// ---- jQuery 子集(同步 API 外观;操作入队,脚本同步段结束后批量 RPC 提交,
//      微任务 flush 保证 getter/setter 链式调用在脚本 await 点前全部发出) ----
const jqHandlers=new Map();
const jqReadyQueue=[];
let jqIdSeq=0;
let ops=[];
let flushPromise=null;
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
function jq(sel){
  if(typeof sel==='function'){jqReadyQueue.push(sel);return;}
  const ref=typeof sel==='string'?{kind:'selector',value:sel}:sel&&sel.kind==='target'?{kind:'target',id:sel.id}:null;
  const localData=sel&&sel.kind==='target'&&sel.data?sel.data:{};
  const coll={};
  coll.text=function(v){if(arguments.length===0)return '';if(ref)enqueue({ref,method:'text',args:[v]});return coll;};
  coll.html=function(v){if(arguments.length===0)return '';if(ref)enqueue({ref,method:'html',args:[v]});return coll;};
  coll.css=function(p,v){if(arguments.length===1)return '';if(ref)enqueue({ref,method:'css',args:[p,v]});return coll;};
  coll.addClass=function(c){if(ref)enqueue({ref,method:'addClass',args:[c]});return coll;};
  coll.removeClass=function(c){if(ref)enqueue({ref,method:'removeClass',args:[c]});return coll;};
  coll.hasClass=function(){return false;};
  coll.data=function(name){return localData[String(name)]??'';};
  coll.val=function(v){if(arguments.length===0)return '';if(ref)enqueue({ref,method:'val',args:[v]});return coll;};
  coll.on=function(evt,fn){if(ref){const id=++jqIdSeq;jqHandlers.set(id,fn);enqueue({ref,method:'on',args:[evt,id]});}return coll;};
  coll.off=function(evt){if(ref)enqueue({ref,method:'off',args:[evt]});return coll;};
  coll.hide=function(){if(ref)enqueue({ref,method:'css',args:['display','none']});return coll;};
  coll.show=function(){if(ref)enqueue({ref,method:'css',args:['display','']});return coll;};
  // 开场界面脚本依赖的动画/属性方法:映射为等价操作(无动画,同步语义与 jQuery 一致)
  coll.slideDown=function(){if(ref)enqueue({ref,method:'css',args:['display','']});return coll;};
  coll.slideUp=function(){if(ref)enqueue({ref,method:'css',args:['display','none']});return coll;};
  coll.fadeIn=function(){if(ref)enqueue({ref,method:'css',args:['display','']});return coll;};
  coll.fadeOut=function(){if(ref)enqueue({ref,method:'css',args:['display','none']});return coll;};
  coll.prop=function(name,value){if(arguments.length===1)return false;if(ref)enqueue({ref,method:'prop',args:[name,value]});return coll;};
  coll.attr=function(name,value){if(arguments.length===1)return '';if(ref)enqueue({ref,method:'attr',args:[name,value]});return coll;};
  coll.trigger=function(evt){if(ref)enqueue({ref,method:'trigger',args:[evt]});return coll;};
  coll.append=function(html){if(ref)enqueue({ref,method:'append',args:[String(html??'')]});return coll;};
  coll.focus=function(){if(ref)enqueue({ref,method:'focus',args:[]});return coll;};
  coll.is=function(){return false;};
  coll.outerHeight=function(){return 0;};
  coll.find=function(){return coll;};
  coll.closest=function(){return coll;};
  coll.remove=function(){if(ref)enqueue({ref,method:'remove',args:[]});return coll;};
  coll.empty=function(){if(ref)enqueue({ref,method:'empty',args:[]});return coll;};
  return coll;
}
const $=jq;
const Mvu=Object.freeze({
  getVariables:()=>structuredClone(variables),
  getMvuData:()=>structuredClone(variables),
  waitGlobalInitialized,
  replaceMvuData:(data)=>{if(!data||typeof data!=='object')return;const stat=data.stat_data&&typeof data.stat_data==='object'?data.stat_data:{};variables.stat_data=stat;variables.display_data=(data.display_data&&typeof data.display_data==='object')?data.display_data:stat;},
  parseMessage:(text)=>{try{const raw=String(text??'');let cleaned=raw;const m=/<UpdateVariable>[\\s\\S]*?<\\/UpdateVariable>/gi.exec(cleaned);if(m)cleaned=cleaned.slice(0,m.index)+cleaned.slice(m.index+m[0].length);return{cleaned,commands:[]};}catch{return{cleaned:String(text??''),commands:[]};}},
  isDuringExtraAnalysis:false,
});
const getAllVariables=()=>structuredClone(variables);
const Dom=Object.freeze({setText:(selector,text)=>rpc('setText',selector,text),setHtml:(selector,html)=>rpc('setHtml',selector,html),setAttribute:(selector,name,value)=>rpc('setAttribute',selector,name,value)});
const fetch=undefined,XMLHttpRequest=undefined,WebSocket=undefined,EventSource=undefined,indexedDB=undefined,caches=undefined,localStorage=__kdMemoryStorage,sessionStorage=__kdMemoryStorage;
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
  getAudioSettings:function(type){const t=type==='ambient'?'ambient':'bgm';return Object.assign({},__kdAudioCache.settings[t]||__kdAudioDefaults[t]);},
  setAudioSettings:function(type,settings){send('audio',{op:'setAudioSettings',args:[type,settings&&typeof settings==='object'?settings:{}]});},
  getCurrentAudio:function(type){const t=type==='ambient'?'ambient':'bgm';const c=__kdAudioCache.current[t]||__kdAudioEmpty();return {src:c.src||'',title:c.title||'',playing:!!c.playing,progress:typeof c.progress==='number'?c.progress:0};}
});
globalThis.TavernHelper=TavernHelper;
(async()=>{try{${escapedCode}\n;await flush();send('done')}catch(error){send('error',{message:error instanceof Error?error.message:String(error)})}})();
`;
}

function queryScoped(container: HTMLElement, selector: string): HTMLElement[] {
  const safe = safeSelector(selector);
  const positional = /:(first|last)\s*$/.exec(safe);
  const base = positional ? safe.slice(0, positional.index).trim() : safe;
  if (!base) throw new Error('选择器无效');
  const matches = Array.from(container.querySelectorAll<HTMLElement>(base));
  if (positional?.[1] === 'first') return matches.slice(0, 1);
  if (positional?.[1] === 'last') return matches.slice(-1);
  return matches;
}

/** jQuery 风格 DOM 操作(经 RPC 转发到宿主容器;事件 target 仅使用本次执行内的受控句柄) */
function applyJq(
  container: HTMLElement,
  ref: JqOperation['ref'],
  method: string,
  args: unknown[],
  targetWindow: Window | null,
  nonce: string,
  targets: Map<number, HTMLElement>,
  nextTargetId: () => number,
  registerEventListener: (element: HTMLElement, eventName: string, listener: EventListener) => void,
): unknown {
  const els = ref.kind === 'selector'
    ? queryScoped(container, ref.value)
    : (targets.get(ref.id) ? [targets.get(ref.id)!] : []);
  const first = els[0];
  switch (method) {
    case 'count':
      return els.length;
    case 'text':
      if (args.length === 0) return first?.textContent ?? '';
      for (const el of els) el.textContent = safeText(args[0]);
      return true;
    case 'html': {
      if (args.length === 0) return first?.innerHTML ?? '';
      const html = safeText(args[0]);
      for (const el of els) {
        el.innerHTML = sanitizeHtml(html, {
          allowedTags: ['span', 'b', 'strong', 'i', 'em', 'small', 'br', 'div', 'p'],
          allowedAttributes: { '*': ['class', 'aria-label', 'style'] },
          allowedSchemes: [],
        });
      }
      return true;
    }
    case 'css':
      if (args.length === 1) return first ? first.style.getPropertyValue(String(args[0])) : '';
      for (const el of els) el.style.setProperty(String(args[0]), String(args[1] ?? ''));
      return true;
    case 'addClass':
      for (const el of els) el.classList.add(...String(args[0] ?? '').split(/\s+/).filter(Boolean));
      return true;
    case 'removeClass':
      for (const el of els) el.classList.remove(...String(args[0] ?? '').split(/\s+/).filter(Boolean));
      return true;
    case 'hasClass':
      return first ? first.classList.contains(String(args[0] ?? '')) : false;
    case 'val':
      if (args.length === 0) return (first as HTMLInputElement | undefined)?.value ?? '';
      for (const el of els) (el as HTMLInputElement).value = String(args[0] ?? '');
      return true;
    case 'prop': {
      // jQuery prop:checkbox/select 的 checked/disabled 等布尔属性
      const name = String(args[0] ?? '');
      const value = args[1];
      if (name === 'checked') {
        for (const el of els) (el as HTMLInputElement).checked = Boolean(value);
      } else if (name === 'disabled') {
        for (const el of els) (el as HTMLInputElement).disabled = Boolean(value);
      } else if (name === 'value') {
        for (const el of els) (el as HTMLInputElement).value = String(value ?? '');
      } else if (value === undefined) {
        return first ? (first as HTMLInputElement)[name as keyof HTMLInputElement] : undefined;
      } else {
        for (const el of els) (el as HTMLInputElement)[name as keyof HTMLInputElement] = value as never;
      }
      return true;
    }
    case 'attr':
      if (args.length === 1) return first ? first.getAttribute(String(args[0])) ?? '' : '';
      for (const el of els) el.setAttribute(String(args[0]), String(args[1] ?? ''));
      return true;
    case 'trigger': {
      // 触发事件(change 等);宿主已注册的监听器(经 on RPC)会收到 dispatchEvent
      const eventName = String(args[0] ?? '');
      for (const el of els) el.dispatchEvent(new Event(eventName, { bubbles: true }));
      return true;
    }
    case 'append': {
      // 追加 HTML(select 填充 option、容器追加节点等);与 innerHTML 一样经 sanitize 清洗
      const html = safeText(args[0]);
      for (const el of els) {
        el.insertAdjacentHTML('beforeend', sanitizeHtml(html, {
          allowedTags: ['span', 'b', 'strong', 'i', 'em', 'small', 'br', 'div', 'p', 'option', 'optgroup', 'label', 'button'],
          allowedAttributes: { '*': ['class', 'value', 'selected', 'disabled', 'aria-label', 'id', 'type', 'name', 'placeholder', 'for'] },
          allowedSchemes: [],
        }));
      }
      return true;
    }
    case 'focus':
      for (const el of els) (el as HTMLElement).focus();
      return true;
    case 'remove':
      for (const el of els) el.remove();
      return true;
    case 'empty':
      for (const el of els) el.innerHTML = '';
      return true;
    case 'off': {
      // 解绑事件:宿主监听器统一由 executeSandboxedCharacterScript 的 cleanup 在容器
      // 卸载/切换时释放;此处仅确认操作合法,避免脚本重复绑定累积(每次执行容器独立)。
      return true;
    }
    case 'on': {
      const eventName = String(args[0] ?? '');
      const jqId = Number(args[1]);
      if (!targetWindow || !eventName || !Number.isFinite(jqId)) return true;
      for (const el of els) {
        const targetId = nextTargetId();
        targets.set(targetId, el);
        const listener = (): void => {
          const data = Object.fromEntries(Object.entries(el.dataset));
          targetWindow.postMessage(
            {
              channel: CHANNEL,
              nonce,
              type: 'jq-event',
              jqId,
              event: { type: eventName },
              target: { kind: 'target', id: targetId, data },
            },
            '*',
          );
        };
        el.addEventListener(eventName, listener);
        registerEventListener(el, eventName, listener);
      }
      return true;
    }
    default:
      throw new Error(`不兼容的角色卡脚本操作: ${method}`);
  }
}

function applyRpc(
  context: SandboxExecutionContext,
  op: string,
  args: unknown[],
): unknown {
  const selector = safeSelector(args[0]);
  const target = context.container.querySelector<HTMLElement>(selector);
  if (!target) throw new Error(`未找到状态栏元素: ${selector}`);
  if (op === 'setText') {
    target.textContent = safeText(args[1]);
    return true;
  }
  if (op === 'setHtml') {
    const html = safeText(args[1]);
    target.innerHTML = sanitizeHtml(html, {
      allowedTags: ['span', 'b', 'strong', 'i', 'em', 'small', 'br'],
      allowedAttributes: { '*': ['class', 'aria-label'] },
      allowedSchemes: [],
    });
    return true;
  }
  if (op === 'setAttribute') {
    const name = String(args[1] ?? '');
    if (!['class', 'title', 'aria-label', 'data-value'].includes(name)) throw new Error('属性不在白名单');
    target.setAttribute(name, safeText(args[2]));
    return true;
  }
  throw new Error(`不兼容的角色卡脚本操作: ${op}`);
}

/** 沙箱启动时经 rpc 拉取的音频全量快照(AudioPlayer 未挂载时回退默认) */
function audioSnapshotValue(): unknown {
  const c = getAudioController();
  const defaults = (type: AudioChannelType): AudioChannelSettings =>
    type === 'bgm'
      ? { enabled: true, mode: 'repeat_all', muted: false, volume: 50 }
      : { enabled: true, mode: 'play_one_and_stop', muted: false, volume: 50 };
  const mk = (type: AudioChannelType) => c
    ? { list: c.getAudioList(type), settings: c.getAudioSettings(type), current: c.getCurrentAudio(type) }
    : { list: [], settings: defaults(type), current: { src: '', title: '', playing: false, progress: 0 } };
  const bgm = mk('bgm');
  const ambient = mk('ambient');
  return {
    list: { bgm: bgm.list, ambient: ambient.list },
    settings: { bgm: bgm.settings, ambient: ambient.settings },
    current: { bgm: bgm.current, ambient: ambient.current },
  };
}

/**
 * 沙箱 TavernHelper 音频 setter 转发(op 白名单;AudioPlayer 未挂载时静默 no-op,
 * 与酒馆音频 slash 命令在无 UI 时的行为一致)。type 非 'ambient' 一律按 'bgm' 处理
 * (对齐酒馆 HF 分发的 switch 语义)。
 */
function handleAudioRpc(op: string, args: unknown[]): void {
  const c = getAudioController();
  if (!c) return;
  const type = (args[0] === 'ambient' ? 'ambient' : 'bgm') as AudioChannelType;
  const second = args[1];
  switch (op) {
    case 'playAudio': {
      const a = second && typeof second === 'object'
        ? second as { title?: unknown; url?: unknown }
        : undefined;
      c.playAudio(
        type,
        a && typeof a.url === 'string'
          ? { title: typeof a.title === 'string' ? a.title : undefined, url: a.url }
          : undefined,
      );
      break;
    }
    case 'pauseAudio':
      c.pauseAudio(type);
      break;
    case 'replaceAudioList':
      c.replaceAudioList(type, Array.isArray(second) ? second as Array<{ title?: string; url: string }> : []);
      break;
    case 'appendAudioList':
      c.appendAudioList(type, Array.isArray(second) ? second as Array<{ title?: string; url: string }> : []);
      break;
    case 'setAudioSettings':
      c.setAudioSettings(type, (second && typeof second === 'object' ? second : {}) as Partial<AudioChannelSettings>);
      break;
    default:
      // 未知音频操作:静默忽略(沙箱侧不感知,保持脚本不中断)
      break;
  }
}

export async function executeSandboxedCharacterScript(
  code: string,
  context: SandboxExecutionContext,
  environment: SandboxEnvironment = { document, window },
): Promise<SandboxCleanup> {
  const nonce = crypto.randomUUID();
  const iframe = environment.document.createElement('iframe');
  for (const [name, value] of Object.entries(sandboxAttributes())) iframe.setAttribute(name, value);
  iframe.setAttribute('tabindex', '-1');
  iframe.style.display = 'none';
  iframe.src = `${SANDBOX_DOCUMENT_URL}#${encodeURIComponent(nonce)}`;

  return new Promise((resolve, reject) => {
    let settled = false;
    let disposed = false;
    const targets = new Map<number, HTMLElement>();
    const eventListeners: Array<{ element: HTMLElement; eventName: string; listener: EventListener }> = [];
    let targetId = 0;
    const cleanup = (): void => {
      if (disposed) return;
      disposed = true;
      environment.window.removeEventListener('message', onMessage as EventListener);
      for (const { element, eventName, listener } of eventListeners) {
        element.removeEventListener(eventName, listener);
      }
      eventListeners.length = 0;
      targets.clear();
      iframe.remove();
    };
    const finish = (error?: Error): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (error && context.container) {
        // 错误提示仅是辅助信息:容器可能已被 Vue 重渲染移除,appendChild 失败不能阻断
        // 后面的 cleanup/reject,否则 Promise 永不 settle。
        try {
          const el = context.container.querySelector<HTMLElement>('.status-card') ?? context.container;
          const msg = environment.document.createElement('p');
          msg.style.cssText = 'color:#f87171;font-size:0.75rem;margin:4px 0 0;opacity:0.8';
          msg.textContent = `[脚本] ${error.message}`;
          el.appendChild(msg);
        } catch {
          /* 忽略:不影响错误传播 */
        }
      }
      if (error) {
        cleanup();
        reject(error);
      } else {
        resolve(cleanup);
      }
    };
    const onMessage = (event: MessageEvent): void => {
      // WebView2 会把跨源 iframe 的 source 包装成不同对象,身份由高熵 nonce + 固定 channel 校验。
      const message = event.data as SandboxRequest;
      if (!message || message.channel !== CHANNEL || message.nonce !== nonce) return;
      try {
        if (message.type === 'ready') {
          // 沙箱文档已从 URL fragment 取得 nonce,认证就绪后才投递一次 boot。
          const boot = { channel: CHANNEL, nonce, type: 'boot', script: sandboxScript(code, nonce, context.variables) };
          if (utf8Bytes(boot) > MAX_MESSAGE_BYTES) throw new Error('角色卡脚本启动消息过大');
          iframe.contentWindow?.postMessage(boot, '*');
        } else if (utf8Bytes(message) > MAX_MESSAGE_BYTES) {
          throw new Error('角色卡脚本消息过大');
        } else if (message.type === 'rpc') {
          // audio-snapshot 是沙箱启动时的音频状态预拉取(非 DOM 操作),
          // 与 applyRpc 的 DOM 白名单并列分发
          const value = message.op === 'audio-snapshot'
            ? audioSnapshotValue()
            : applyRpc(context, String(message.op ?? ''), message.args ?? []);
          iframe.contentWindow?.postMessage({
            channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: true, value: cloneData(value),
          }, '*');
        } else if (message.type === 'audio') {
          handleAudioRpc(String(message.op ?? ''), message.args ?? []);
        } else if (message.type === 'batch') {
          // 每批必须有 RPC id,沙箱收到 ack 后才允许报告完成。
          if (!Number.isFinite(message.id)) throw new Error('角色卡脚本 batch 缺少 id');
          const ops = Array.isArray(message.ops) ? message.ops : [];
          for (const operation of ops) {
            try {
              applyJq(
                context.container,
                operation.ref,
                String(operation.method ?? ''),
                Array.isArray(operation.args) ? operation.args : [],
                iframe.contentWindow,
                nonce,
                targets,
                () => ++targetId,
                (element, eventName, listener) => eventListeners.push({ element, eventName, listener }),
              );
            } catch (error) {
              iframe.contentWindow?.postMessage({
                channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: false, error: error instanceof Error ? error.message : String(error),
              }, '*');
              return;
            }
          }
          iframe.contentWindow?.postMessage({
            channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: true, value: true,
          }, '*');
        } else if (message.type === 'warn') {
          console.warn('[kedai-mvu]', message.message ?? '');
        } else if (message.type === 'done') {
          finish();
        } else if (message.type === 'error') {
          finish(new Error(`角色卡脚本不兼容或执行失败: ${message.message ?? '未知错误'}`));
        }
      } catch (error) {
        const text = error instanceof Error ? error.message : String(error);
        if (message.type === 'rpc') {
          iframe.contentWindow?.postMessage({
            channel: CHANNEL, nonce, type: 'rpc-result', id: message.id, ok: false, error: text,
          }, '*');
        } else {
          finish(new Error(text));
        }
      }
    };
    const timer = setTimeout(() => finish(new Error('角色卡脚本执行超时')), environment.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    environment.window.addEventListener('message', onMessage as EventListener);
    environment.document.body.appendChild(iframe);
  });
}
