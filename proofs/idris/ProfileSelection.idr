module ProfileSelection

%default total

public export
data Profile
  = P53Elan | X230Touchpad | ThinkPadTrackpoint | GenericTouchpad

public export
data Evidence
  = P53ElanDevice | X230Device | ThinkPadTrackpointDevice
  | GenericTouchpadDevice | UnknownDevice

public export
select : Evidence -> Maybe Profile
select P53ElanDevice = Just P53Elan
select X230Device = Just X230Touchpad
select ThinkPadTrackpointDevice = Just ThinkPadTrackpoint
select GenericTouchpadDevice = Just GenericTouchpad
select UnknownDevice = Nothing

public export
data Selected : Evidence -> Profile -> Type where
  Chosen : select evidence = Just profile -> Selected evidence profile

justInjective : {left, right : value} -> Just left = Just right -> left = right
justInjective Refl = Refl

public export
selectedUnique : {evidence : Evidence} -> {left, right : Profile} ->
                 Selected evidence left -> Selected evidence right -> left = right
selectedUnique (Chosen leftSelected) (Chosen rightSelected) =
  justInjective (trans (sym leftSelected) rightSelected)

public export
selectIsTotal : (evidence : Evidence) -> Maybe Profile
selectIsTotal evidence = select evidence
