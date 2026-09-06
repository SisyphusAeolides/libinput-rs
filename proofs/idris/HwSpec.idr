module HwSpec

%default total

public export
data Kind
  = Keyboard | Key | Mouse | Touchpad | Touchscreen
  | Tablet | TabletPad | Joystick | Accelerometer | Switch | Phantom

public export
record CapEvidence where
  constructor MkCaps
  hasKey, hasRelative, hasAbsolute, hasSwitch : Bool
  absoluteXY, multitouch, finger, touch, pen, buttonLeft : Bool
  pointerProperty, directProperty : Bool
  keyCount : Nat

public export
record UdevEvidence where
  constructor MkUdev
  ignore, keyboard, key, mouse, touchpad, touchscreen : Bool
  tablet, tabletPad, joystick, accelerometer, switch : Bool
  seat : String

classifyCaps : CapEvidence -> Kind
classifyCaps caps =
  if caps.pen && caps.absoluteXY then Tablet
  else if (caps.finger || (caps.touch && caps.pointerProperty && not caps.directProperty))
          && caps.absoluteXY then Touchpad
  else if (caps.directProperty || (caps.touch && caps.multitouch))
          && caps.absoluteXY then Touchscreen
  else if caps.hasRelative && caps.buttonLeft then Mouse
  else if caps.hasKey && caps.keyCount > 20 then Keyboard
  else if caps.hasKey && caps.keyCount > 0 then Key
  else if caps.hasSwitch then Switch
  else Phantom

public export
classify : UdevEvidence -> CapEvidence -> Kind
classify udev caps =
  if udev.ignore then Phantom
  else if udev.touchpad then Touchpad
  else if udev.touchscreen then Touchscreen
  else if udev.tablet then Tablet
  else if udev.tabletPad then TabletPad
  else if udev.joystick then Joystick
  else if udev.accelerometer then Accelerometer
  else if udev.mouse then Mouse
  else if udev.keyboard then Keyboard
  else if udev.key then Key
  else if udev.switch then Switch
  else classifyCaps caps

public export
data LiveKind
  = LiveKeyboard | LiveKey | LiveMouse | LiveTouchpad | LiveTouchscreen
  | LiveTablet | LiveTabletPad | LiveJoystick | LiveAccelerometer | LiveSwitch

public export
accept : Kind -> Maybe LiveKind
accept Keyboard = Just LiveKeyboard
accept Key = Just LiveKey
accept Mouse = Just LiveMouse
accept Touchpad = Just LiveTouchpad
accept Touchscreen = Just LiveTouchscreen
accept Tablet = Just LiveTablet
accept TabletPad = Just LiveTabletPad
accept Joystick = Just LiveJoystick
accept Accelerometer = Just LiveAccelerometer
accept Switch = Just LiveSwitch
accept Phantom = Nothing

public export
data Action = Add | Remove | Change

public export
Registry : Type
Registry = List (String, LiveKind)

removeNode : String -> Registry -> Registry
removeNode node [] = []
removeNode node ((name, kind) :: rest) =
  if node == name then removeNode node rest
  else (name, kind) :: removeNode node rest

public export
apply : Action -> String -> Kind -> Registry -> Registry
apply Remove node _ registry = removeNode node registry
apply Add node kind registry =
  case accept kind of
    Nothing => registry
    Just live => (node, live) :: removeNode node registry
apply Change node kind registry =
  let cleaned = removeNode node registry in
  case accept kind of
    Nothing => cleaned
    Just live => (node, live) :: cleaned

public export
phantomCannotBind : (node : String) -> (registry : Registry) ->
                    apply Add node Phantom registry = registry
phantomCannotBind node registry = Refl

public export
seatOk : String -> String -> Bool
seatOk wanted actual =
  let wantedSeat = if wanted == "" then "seat0" else wanted
      actualSeat = if actual == "" then "seat0" else actual
   in wantedSeat == actualSeat

public export
detectOne : String -> UdevEvidence -> CapEvidence -> Maybe LiveKind
detectOne wanted udev caps =
  if seatOk wanted udev.seat then accept (classify udev caps) else Nothing
