{-# OPTIONS --safe #-}

module ProfileSelection where

data _≡_ {A : Set} (x : A) : A → Set where
  refl : x ≡ x

sym : {A : Set} {x y : A} → x ≡ y → y ≡ x
sym refl = refl

trans : {A : Set} {x y z : A} → x ≡ y → y ≡ z → x ≡ z
trans refl refl = refl

data Maybe (A : Set) : Set where
  nothing : Maybe A
  just : A → Maybe A

data Profile : Set where
  p53-elan x230-touchpad thinkpad-trackpoint generic-touchpad : Profile

data Evidence : Set where
  p53-elan-device x230-device thinkpad-trackpoint-device : Evidence
  generic-touchpad-device unknown : Evidence

select : Evidence → Maybe Profile
select p53-elan-device = just p53-elan
select x230-device = just x230-touchpad
select thinkpad-trackpoint-device = just thinkpad-trackpoint
select generic-touchpad-device = just generic-touchpad
select unknown = nothing

data Matches : Evidence → Profile → Set where
  p53-matches : Matches p53-elan-device p53-elan
  x230-matches : Matches x230-device x230-touchpad
  trackpoint-matches : Matches thinkpad-trackpoint-device thinkpad-trackpoint
  generic-matches : Matches generic-touchpad-device generic-touchpad

data Applies (evidence : Evidence) (profile : Profile) : Set where
  selected : select evidence ≡ just profile → Applies evidence profile

just-injective : {left right : Profile} → just left ≡ just right → left ≡ right
just-injective refl = refl

selected-matches : {evidence : Evidence} {profile : Profile} →
  Applies evidence profile → Matches evidence profile
selected-matches {p53-elan-device} {p53-elan} (selected refl) = p53-matches
selected-matches {x230-device} {x230-touchpad} (selected refl) = x230-matches
selected-matches {thinkpad-trackpoint-device} {thinkpad-trackpoint} (selected refl) =
  trackpoint-matches
selected-matches {generic-touchpad-device} {generic-touchpad} (selected refl) =
  generic-matches

no-conflicting-applies : {evidence : Evidence} {left right : Profile} →
  Applies evidence left → Applies evidence right → left ≡ right
no-conflicting-applies (selected left) (selected right) =
  just-injective (trans (sym left) right)
