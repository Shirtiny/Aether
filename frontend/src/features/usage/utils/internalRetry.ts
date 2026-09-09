import type { InternalRetryInfo } from '@/types/internalRetry'

export function internalRetryLabel(info?: InternalRetryInfo | null): string {
  if (!info) return '内部重试未记录'
  if (info.retry_count === 0) return info.complete ? '未发生内部重试' : '内部重试记录不完整'
  const outcome: Record<InternalRetryInfo['outcome'], string> = {
    succeeded: '最终成功', failed: '最终失败', cancelled: '请求已取消',
    in_progress: '请求进行中', not_retried: '结果待确认', unknown: '结果未记录',
  }
  const count = `${info.complete ? '' : '至少 '}${info.retry_count}`
  return `内部重试 ${count} 次 · ${outcome[info.outcome] ?? '结果未记录'}`
}

export function internalRetryStopReason(reason?: string | null): string {
  const reasons: Record<string, string> = {
    budget_exhausted: '重试次数已耗尽',
    retry_after_rejected: '上游 Retry-After 超出限制或无法解析',
    content_started: '实质内容开始，关闭重放窗口',
    reasoning_started: '推理内容开始，关闭重放窗口',
    tool_started: '工具活动开始，禁止重放',
    empty_output_event: '空输出事件，按现有保守规则关闭重放窗口',
    unknown_event: '未知事件，保守关闭重放窗口',
    malformed_event: '事件格式无法确认，禁止重放',
    buffer_limit: '开场缓冲达到上限，停止缓存和重放',
    non_retryable_error: '错误不符合过载重试条件',
    client_cancelled: '下游断开或取消，停止重试',
    not_sse: '非 SSE 响应，结束流式过载检测',
    terminal_event: '收到终止事件，关闭重放窗口',
    response_completed: '同步响应结束',
    stream_ended_before_content: '开场期间流结束，交由既有断流处理',
    stream_ended: '流结束',
  }
  return reason ? (reasons[reason] ?? '未识别的退出原因') : '重试检测尚未结束'
}
