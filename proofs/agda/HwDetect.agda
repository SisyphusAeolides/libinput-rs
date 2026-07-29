{-# OPTIONS --safe #-}

module HwDetect where

data _≡_ {A : Set} (x : A) : A → Set where
  refl : x ≡ x

data Capability : Set where
  key relative absolute switch pointer direct touch finger pen : Capability

data List (A : Set) : Set where
  [] : List A
  _∷_ : A → List A → List A

infixr 5 _∷_ _++_

_++_ : {A : Set} → List A → List A → List A
[] ++ ys = ys
(x ∷ xs) ++ ys = x ∷ (xs ++ ys)

data _∈_ {A : Set} (x : A) : List A → Set where
  here : {xs : List A} → x ∈ (x ∷ xs)
  there : {y : A} {xs : List A} → x ∈ xs → x ∈ (y ∷ xs)

Capabilities = List Capability

_≤ᶜ_ : Capabilities → Capabilities → Set
xs ≤ᶜ ys = {capability : Capability} → capability ∈ xs → capability ∈ ys

_⊔_ : Capabilities → Capabilities → Capabilities
_⊔_ = _++_

join-left : (xs ys : Capabilities) → xs ≤ᶜ (xs ⊔ ys)
join-left [] ys ()
join-left (x ∷ xs) ys here = here
join-left (x ∷ xs) ys (there member) = there (join-left xs ys member)

join-right : (xs ys : Capabilities) → ys ≤ᶜ (xs ⊔ ys)
join-right [] ys member = member
join-right (x ∷ xs) ys member = there (join-right xs ys member)

join-least : (xs ys upper : Capabilities) →
  xs ≤ᶜ upper → ys ≤ᶜ upper → (xs ⊔ ys) ≤ᶜ upper
join-least [] ys upper left right member = right member
join-least (x ∷ xs) ys upper left right here = left here
join-least (x ∷ xs) ys upper left right (there member) =
  join-least xs ys upper (λ evidence → left (there evidence)) right member

data DeviceState : Set where
  listed opened classified announced removed : DeviceState

data Step : DeviceState → DeviceState → Set where
  open-device : Step listed opened
  deny : Step listed removed
  classify-device : Step opened classified
  reject : Step opened removed
  announce-device : Step classified announced
  unplug-opened : Step opened removed
  unplug-classified : Step classified removed
  unplug-announced : Step announced removed

data Reachable : DeviceState → Set where
  coldplug : Reachable listed
  advance : {from to : DeviceState} → Reachable from → Step from to → Reachable to

announce-predecessor : {state : DeviceState} → Step state announced → state ≡ classified
announce-predecessor announce-device = refl

removed-is-terminal : {state : DeviceState} → Step removed state → state ≡ removed
removed-is-terminal ()
