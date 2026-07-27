module RestrictedDiscovery

%default total

data DirectAccess = Denied | Allowed
data DirectoryEntry = Absent | EventNode
data ScanResult = Missing | Candidate
data RestrictedAccess = Refused | Granted
data OpenResult = Closed | Opened

scanByDirectOpen : DirectAccess -> ScanResult
scanByDirectOpen Denied = Missing
scanByDirectOpen Allowed = Candidate

scanByName : DirectoryEntry -> ScanResult
scanByName Absent = Missing
scanByName EventNode = Candidate

openRestricted : ScanResult -> RestrictedAccess -> OpenResult
openRestricted Candidate Granted = Opened
openRestricted _ _ = Closed

permissionIndependentDiscovery : (access : DirectAccess) ->
                                 scanByName EventNode = Candidate
permissionIndependentDiscovery _ = Refl

legacyDropsRestrictedNode : scanByDirectOpen Denied = Missing
legacyDropsRestrictedNode = Refl

restrictedCallbackOpensCandidate :
  openRestricted (scanByName EventNode) Granted = Opened
restrictedCallbackOpensCandidate = Refl

restrictedCallbackDenialFailsOpen :
  openRestricted (scanByName EventNode) Refused = Closed
restrictedCallbackDenialFailsOpen = Refl
