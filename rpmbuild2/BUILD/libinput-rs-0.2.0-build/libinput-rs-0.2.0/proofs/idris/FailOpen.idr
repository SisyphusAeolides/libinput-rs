module FailOpen

%default total

data Sink = Absent | Ready
data Grab = Released | Grabbed

data Runtime : Sink -> Grab -> Type where
  Boot : Runtime Absent Released
  Prepared : Runtime Ready Released
  Active : Runtime Ready Grabbed

prepare : Runtime Absent Released -> Runtime Ready Released
prepare Boot = Prepared

grab : Runtime Ready Released -> Runtime Ready Grabbed
grab Prepared = Active

release : Runtime Ready Grabbed -> Runtime Ready Released
release Active = Prepared

forwardFailure : Runtime Ready Grabbed -> Runtime Ready Released
forwardFailure = release

data CanGrab : Sink -> Type where
  SinkReady : CanGrab Ready

noGrabWithoutSink : CanGrab Absent -> Void
noGrabWithoutSink SinkReady impossible
