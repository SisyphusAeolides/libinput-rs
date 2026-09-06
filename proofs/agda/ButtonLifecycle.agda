module ButtonLifecycle where

open import Agda.Builtin.Equality

data State : Set where
  up down : State

data Input : Set where
  press release disconnect : Input

data Output : Set where
  silent pressed released : Output

record Result : Set where
  constructor result
  field
    state : State
    output : Output

step : State -> Input -> Result
step up press = result down pressed
step down press = result down silent
step up release = result up silent
step down release = result up released
step up disconnect = result up silent
step down disconnect = result up released

duplicatePressIsSilent : step down press ≡ result down silent
duplicatePressIsSilent = refl

unmatchedReleaseIsSilent : step up release ≡ result up silent
unmatchedReleaseIsSilent = refl

lostReleaseRecovery : step down release ≡ result up released
lostReleaseRecovery = refl

disconnectBalancesPress : step down disconnect ≡ result up released
disconnectBalancesPress = refl
