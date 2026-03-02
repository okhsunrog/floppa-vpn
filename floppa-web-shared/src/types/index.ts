export * from './peer'

// Re-export API types from generated client for convenience
export type {
  TelegramAuthData,
  AuthResponse,
  AuthUserInfo,
  PublicConfig,
  MeResponse,
  MySubscription,
  MyPeer,
  CreatePeerResponse,
  CreatePeerRequest,
  Stats,
  UserSummary,
  UserDetail,
  PeerDetail,
  SubscriptionDetail,
  PeerSummary,
  Plan,
  CreatePlanRequest,
  UpdatePlanRequest,
  SetSubscriptionRequest,
  MiniAppAuthRequest,
} from '../client/types.gen'
