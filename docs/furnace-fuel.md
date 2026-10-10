# Furnace fuel

## What it is

Burn times for furnace-family fuel, `lodestone_server::furnace::base_burn_duration`. The smoker and blast furnace halve the result.

## How it works

The 26.3 reference registers fuel as a per-item component, so the table is a name-pattern restatement. Wood families are derived, not listed: `wood_species` accepts `<species>_<kind>` when `<species>_planks` is a real item, so a new wood (poplar) or chest boat is fuel without a code change; the non-flammable nether woods are excluded first. Dyed items (wool, wool stairs and slabs, carpets, cushions, banners) match on suffix, and carpets also require a matching dyed wool so moss carpets stay inert.

`furnace_fuel_reference.txt` holds every fuel item and its burn ticks as read from the reference item registrations. `every_item_burns_for_its_reference_time` runs the whole item registry against it, so both a missing fuel and an over-granted one fail.

## How to change it

Add a concrete entry or suffix rule in `base_burn_duration`, then regenerate the reference rows from the new registrations and rerun the test.

## Configuration

None.

## Dependencies

`lodestone-data` item registry.
