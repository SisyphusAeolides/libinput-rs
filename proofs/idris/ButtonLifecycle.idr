module ButtonLifecycle

%default total

data ButtonState = Up | Down
data ButtonInput = Press | Release | Disconnect
data ButtonOutput = Silent | Pressed | Released

step : ButtonState -> ButtonInput -> (ButtonState, ButtonOutput)
step Up Press = (Down, Pressed)
step Down Press = (Down, Silent)
step Up Release = (Up, Silent)
step Down Release = (Up, Released)
step Up Disconnect = (Up, Silent)
step Down Disconnect = (Up, Released)

duplicatePressIsSilent : step Down Press = (Down, Silent)
duplicatePressIsSilent = Refl

unmatchedReleaseIsSilent : step Up Release = (Up, Silent)
unmatchedReleaseIsSilent = Refl

lostReleaseRecovery : step Down Release = (Up, Released)
lostReleaseRecovery = Refl

disconnectBalancesPress : step Down Disconnect = (Up, Released)
disconnectBalancesPress = Refl
