module RestrictedDiscovery where

data DirectAccess : Set where
  denied allowed : DirectAccess

data DirectoryEntry : Set where
  absent eventNode : DirectoryEntry

data ScanResult : Set where
  missing candidate : ScanResult

data RestrictedAccess : Set where
  refused granted : RestrictedAccess

data OpenResult : Set where
  closed opened : OpenResult

scanByDirectOpen : DirectAccess -> ScanResult
scanByDirectOpen denied = missing
scanByDirectOpen allowed = candidate

scanByName : DirectoryEntry -> ScanResult
scanByName absent = missing
scanByName eventNode = candidate

openRestricted : ScanResult -> RestrictedAccess -> OpenResult
openRestricted candidate granted = opened
openRestricted _ _ = closed

data _≡_ {A : Set} (x : A) : A -> Set where
  refl : x ≡ x

permissionIndependentDiscovery :
  (access : DirectAccess) -> scanByName eventNode ≡ candidate
permissionIndependentDiscovery _ = refl

legacyDropsRestrictedNode : scanByDirectOpen denied ≡ missing
legacyDropsRestrictedNode = refl

restrictedCallbackOpensCandidate :
  openRestricted (scanByName eventNode) granted ≡ opened
restrictedCallbackOpensCandidate = refl

restrictedCallbackDenialFailsOpen :
  openRestricted (scanByName eventNode) refused ≡ closed
restrictedCallbackDenialFailsOpen = refl
