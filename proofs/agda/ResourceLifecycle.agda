module ResourceLifecycle where

data Backend : Set where
  path udev : Backend

data Restricted : Set where
  fdOpen fdClosed : Restricted

data Device : Restricted -> Set where
  acquired : Device fdOpen
  finished : Device fdClosed

reject : Device fdOpen -> Device fdClosed
reject acquired = finished

remove : Device fdOpen -> Device fdClosed
remove acquired = finished

data CanHotplug : Backend -> Set where
  udevHotplug : CanHotplug udev

data Empty : Set where

noPathHotplug : CanHotplug path -> Empty
noPathHotplug ()
