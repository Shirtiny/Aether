import type { Component } from 'vue'
import {
  Rocket,
  Network,
  BookOpen,
  Target,
  Settings,
  Blocks,
  HelpCircle
} from 'lucide-vue-next'

// 导航配置
export interface GuideNavItem {
  id: string
  name: string
  path: string
  icon: Component
  description?: string
  subItems?: { name: string; hash: string }[]
}

export const guideNavItems: GuideNavItem[] = [
  {
    id: 'overview',
    name: '快速开始',
    path: '/guide',
    icon: Rocket,
    description: '部署后的配置指南',
    subItems: [
      { name: '部署', hash: '#production' },
      { name: '配置流程', hash: '#config-steps' },
      { name: '反向代理', hash: '#reverse-proxy' },
      { name: '异步任务', hash: '#async-tasks' },
      { name: '代理配置', hash: '#proxy-config' }
    ]
  },
  {
    id: 'architecture',
    name: '架构说明',
    path: '/guide/architecture',
    icon: Network,
    description: '系统架构'
  },
  {
    id: 'concepts',
    name: '相关概念',
    path: '/guide/concepts',
    icon: BookOpen,
    description: '核心概念',
    subItems: [
      { name: '创建统一模型', hash: '#create-model' },
      { name: '添加提供商', hash: '#add-provider' },
      { name: '添加端点', hash: '#add-endpoint' },
      { name: '添加密钥', hash: '#add-key' },
      { name: '模型权限', hash: '#model-permission' },
      { name: '关联模型', hash: '#link-model' },
      { name: '模型映射', hash: '#model-mapping' },
      { name: '反向代理', hash: '#reverse-proxy' },
      { name: '优先级管理', hash: '#priority-management' }
    ]
  },
  {
    id: 'strategy',
    name: '关键策略',
    path: '/guide/strategy',
    icon: Target,
    description: '关键策略',
    subItems: [
      { name: '请求体记录', hash: '#request-logging' },
      { name: '调度模式', hash: '#scheduling' },
      { name: '访问限制', hash: '#rate-limit' },
      { name: '请求体清理', hash: '#payload-cleanup' },
      { name: '定时任务', hash: '#cron-tasks' }
    ]
  },
  {
    id: 'advanced',
    name: '高级功能',
    path: '/guide/advanced',
    icon: Settings,
    description: '高级功能',
    subItems: [
      { name: '格式转换', hash: '#format-conversion' },
      { name: '流式/非流式', hash: '#stream-policy' },
      { name: '请求头/体编辑', hash: '#header-body-edit' },
      { name: '模型映射', hash: '#model-mapping' },
      { name: '正则映射', hash: '#regex-mapping' },
      { name: '余额监控', hash: '#balance-monitor' },
      { name: '配置导入/出', hash: '#config-export' },
      { name: '锁定用户密钥', hash: '#lock-key' }
    ]
  },
  {
    id: 'modules',
    name: '模块管理',
    path: '/guide/modules',
    icon: Blocks,
    description: '模块管理',
    subItems: [
      { name: '访问令牌', hash: '#management-tokens' },
      { name: '邮件配置', hash: '#email-config' },
      { name: 'OAuth登录', hash: '#oauth-login' },
      { name: 'LDAP认证', hash: '#ldap-auth' }
    ]
  },
  {
    id: 'faq',
    name: '常见问题',
    path: '/guide/faq',
    icon: HelpCircle,
    description: '常见问题'
  }
]

// 样式类常量 - 使用 Literary Tech 主题
export const panelClasses = {
  card: 'literary-card rounded-2xl backdrop-blur-sm transition-all duration-300',
  cardHover: 'hover:-translate-y-1 hover:shadow-lg dark:hover:shadow-[var(--book-cloth)]/10 shadow-[var(--book-cloth)]/10',
  section: 'literary-surface-inset bg-white/40 dark:bg-black/20 backdrop-blur-md rounded-xl md:rounded-2xl p-5 md:p-8 transition-colors',
  commandPanel: 'literary-surface-elevated rounded-xl overflow-hidden shadow-sm backdrop-blur-md',
  configPanel: 'literary-surface-elevated rounded-xl overflow-hidden',
  panelHeader: 'px-4 py-3 border-b literary-border bg-[var(--color-background-soft)]/50',
  codeBody: 'p-0',
  badge: 'literary-badge bg-[var(--color-background)] rounded-full px-3 py-1.5',
  badgeBlue: 'inline-flex items-center gap-1.5 rounded-full bg-blue-500/10 dark:bg-blue-500/20 border border-blue-500/20 dark:border-blue-500/40 px-2 py-0.5 text-xs font-medium text-blue-600 dark:text-blue-400',
  badgeGreen: 'inline-flex items-center gap-1.5 rounded-full bg-green-500/10 dark:bg-green-500/20 border border-green-500/20 dark:border-green-500/40 px-2 py-0.5 text-xs font-medium text-green-600 dark:text-green-400',
  badgeYellow: 'inline-flex items-center gap-1.5 rounded-full bg-yellow-500/10 dark:bg-yellow-500/20 border border-yellow-500/20 dark:border-yellow-500/40 px-2 py-0.5 text-xs font-medium text-yellow-600 dark:text-yellow-400',
  badgePurple: 'inline-flex items-center gap-1.5 rounded-full bg-purple-500/10 dark:bg-purple-500/20 border border-purple-500/20 dark:border-purple-500/40 px-2 py-0.5 text-xs font-medium text-purple-600 dark:text-purple-400',
  iconButtonSmall: [
    'flex items-center justify-center rounded-lg border h-8 w-8',
    'literary-border',
    'bg-transparent',
    'text-[var(--color-text)]',
    'transition hover:bg-[var(--color-background-soft)]'
  ].join(' ')
} as const
