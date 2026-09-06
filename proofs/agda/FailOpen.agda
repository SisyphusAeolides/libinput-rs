module FailOpen where

data Sink : Set where
  absent ready : Sink

data Grab : Set where
  released grabbed : Grab

data Runtime : Sink -> Grab -> Set where
  boot     : Runtime absent released
  prepared : Runtime ready released
  active   : Runtime ready grabbed

prepare : Runtime absent released -> Runtime ready released
prepare boot = prepared

grab : Runtime ready released -> Runtime ready grabbed
grab prepared = active

release : Runtime ready grabbed -> Runtime ready released
release active = prepared

forwardFailure : Runtime ready grabbed -> Runtime ready released
forwardFailure = release

data CanGrab : Sink -> Set where
  sinkReady : CanGrab ready

noGrabWithoutSink : CanGrab absent -> Runtime absent released
noGrabWithoutSink ()
