import { invoke } from '@tauri-apps/api/core'
import type { UiActionReview } from './intelligent-ui'

export const claimUiAction = (reviewId: string) => invoke<UiActionReview>('intelligent_ui_request', {method: 'action.claim', params: {reviewId}})
