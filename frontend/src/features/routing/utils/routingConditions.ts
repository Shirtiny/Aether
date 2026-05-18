export type RoutingConditionOp = 'eq' | 'ne' | 'in' | 'contains' | 'exists' | 'matches'

export interface RoutingConditionLeaf {
  field: string
  op: RoutingConditionOp
  value?: unknown
}

export interface RoutingConditionGroup {
  all?: RoutingCondition[]
  any?: RoutingCondition[]
  not?: RoutingCondition
}

export type RoutingCondition = RoutingConditionLeaf | RoutingConditionGroup

export const routingConditionFieldLabels: Record<string, string> = {
  model: '模型',
  api_format: 'API 格式',
  user_id: '用户',
  api_key_id: 'API Key',
}

export const routingConditionOpLabels: Record<RoutingConditionOp, string> = {
  eq: '等于',
  ne: '不等于',
  in: '包含于',
  contains: '包含',
  exists: '存在',
  matches: '匹配',
}

export function isConditionLeaf(condition: RoutingCondition): condition is RoutingConditionLeaf {
  return typeof (condition as RoutingConditionLeaf).field === 'string'
}

export function summarizeRoutingCondition(condition: RoutingCondition): string {
  if (isConditionLeaf(condition)) {
    const field = routingConditionFieldLabels[condition.field] ?? condition.field
    const op = routingConditionOpLabels[condition.op] ?? condition.op
    return `${field} ${op} ${formatConditionValue(condition.value)}`
  }

  if (condition.all?.length) {
    return condition.all.map(summarizeRoutingCondition).join(' 且 ')
  }

  if (condition.any?.length) {
    return condition.any.map(summarizeRoutingCondition).join(' 或 ')
  }

  if (condition.not) {
    return `非 ${summarizeRoutingCondition(condition.not)}`
  }

  return '无条件'
}

function formatConditionValue(value: unknown): string {
  if (Array.isArray(value)) {
    return value.map(formatConditionValue).join(', ')
  }

  if (value === undefined || value === null) {
    return ''
  }

  if (typeof value === 'object') {
    return JSON.stringify(value)
  }

  return String(value)
}
