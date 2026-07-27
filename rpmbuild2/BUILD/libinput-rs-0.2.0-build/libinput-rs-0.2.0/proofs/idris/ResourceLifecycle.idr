module ResourceLifecycle

%default total

data Backend = Path | Udev
data Restricted = Open | Closed

data Device : Restricted -> Type where
  Acquired : Device Open
  Finished : Device Closed

reject : Device Open -> Device Closed
reject Acquired = Finished

remove : Device Open -> Device Closed
remove Acquired = Finished

data CanHotplug : Backend -> Type where
  UdevHotplug : CanHotplug Udev

noPathHotplug : CanHotplug Path -> Void
noPathHotplug UdevHotplug impossible
