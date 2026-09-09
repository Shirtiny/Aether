export interface InternalRetryFailure {
  attempt: number
  status_code: number
  reason: string
  planned_wait_ms: number | null
  wait_ms: number | null
  retry_started: boolean
}
export interface InternalRetryAttempt {
  candidate_index: number | null
  retry_index: number | null
  retry_count: number
  failures: InternalRetryFailure[]
  stop_reason: string | null
  stop_event_type: string | null
}
export interface InternalRetryInfo {
  version: 1
  scope: 'aether'
  retry_count: number
  complete: boolean
  outcome: 'succeeded' | 'failed' | 'cancelled' | 'in_progress' | 'not_retried' | 'unknown'
  attempts: InternalRetryAttempt[]
}
