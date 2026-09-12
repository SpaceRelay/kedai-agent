// chat store 测试:swipeMessage 成功后向卡级脚本沙箱广播 message_swiped(楼层序号)。
// 只 mock api 层与沙箱广播函数,store 内部逻辑(消息加载/替换)走真实代码。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

// node 环境无 localStorage,补内存桩(与 character.test.ts 同款)
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

vi.mock('../api', () => ({
  fetchHistory: vi.fn(),
  swipeMessage: vi.fn(),
  getSessionTotalTokens: vi.fn(),
  getGlobalTotalTokens: vi.fn(),
  countTokens: vi.fn(),
  streamChat: vi.fn(),
  stopChat: vi.fn(),
}));

vi.mock('../characterScriptSandbox', () => ({
  broadcastMvuUpdate: vi.fn(),
  broadcastCardEvent: vi.fn(),
}));

import * as api from '../api';
import { broadcastCardEvent } from '../characterScriptSandbox';
import { useChatStore } from './chat';

const mocked = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
const mockedBroadcast = broadcastCardEvent as unknown as ReturnType<typeof vi.fn>;

const HISTORY = [
  {
    id: 5,
    role: 'assistant',
    content: '开场v0',
    extra: {
      swipes: [
        { swipe_id: 0, content: '开场v0', ts: 1 },
        { swipe_id: 1, content: '开场v1', ts: 2 },
      ],
      swipe_id: 0,
    },
  },
  { id: 6, role: 'user', content: '你好', extra: {} },
];

beforeEach(() => {
  memStorage.clear();
  vi.clearAllMocks();
  setActivePinia(createPinia());
  mocked.fetchHistory.mockResolvedValue(HISTORY.map((m) => ({ ...m })));
  mocked.getSessionTotalTokens.mockResolvedValue(0);
  mocked.getGlobalTotalTokens.mockResolvedValue(0);
  mocked.countTokens.mockResolvedValue(0);
});

describe('swipeMessage 广播(卡级脚本事件流)', () => {
  it('切换成功后广播 message_swiped,payload 为 0-based 楼层序号', async () => {
    mocked.swipeMessage.mockResolvedValue({ content: '开场v1', swipe_id: 1 });
    const store = useChatStore();
    await store.switchSession('s1');
    expect(store.messages).toHaveLength(2);

    await store.swipeMessage(5, 1);

    // 消息内容已切换
    expect(store.messages[0].content).toBe('开场v1');
    expect(store.messages[0].extra.swipe_id).toBe(1);
    // 广播:事件名 + 楼层序号(消息 id=5 位于数组下标 0;舰娘卡脚本据此判定第一条开场白)
    expect(mockedBroadcast).toHaveBeenCalledTimes(1);
    expect(mockedBroadcast).toHaveBeenCalledWith('message_swiped', 0);
  });

  it('中间楼层广播其数组下标(非消息 id)', async () => {
    mocked.swipeMessage.mockResolvedValue({ content: 'vX', swipe_id: 2 });
    const store = useChatStore();
    await store.switchSession('s1');

    await store.swipeMessage(6, 2);
    expect(mockedBroadcast).toHaveBeenCalledWith('message_swiped', 1);
  });

  it('后端失败不广播', async () => {
    mocked.swipeMessage.mockRejectedValue(new Error('网络错误'));
    const store = useChatStore();
    await store.switchSession('s1');

    await store.swipeMessage(5, 1);
    expect(mockedBroadcast).not.toHaveBeenCalled();
  });
});
