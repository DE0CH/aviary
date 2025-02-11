# OcTextInput component

## Description

`OcTextInput`s allow users to provide text input. Commonly used when the expected input is short.
This component has a range of options and supports several input types, including numbers.
For longer input, use the `OcTextarea` component.

## Accessibility

The label is required and represents the name of the input.

The description-message can be used additionally to give further information about the input field. When a
description is given, it will be referenced via the `aria-describedby` property automatically.
An error or warning will replace the description as well as the `aria-describedby` property until the error
or warning is fixed.

## Examples

### Default

The default and most simple use case involves a `v-model` and a `label`.

<oc-text-input v-model="name" label="Your name" />

### Disabled

<oc-text-input disabled label="Disabled" value="I am disabled" />

### Input Types

The following input types ares supported.

<oc-text-input class="oc-mb-s" label="Text" />
<oc-text-input class="oc-mb-s" read-only="true" label="Read only" value="I am read only" />
<oc-text-input class="oc-mb-s" type="number" label="Number" />
<oc-text-input class="oc-mb-s" type="email" label="Email" />
<oc-text-input class="oc-mb-s" type="password" label="Password" />

### Model

Two way binding a model via `v-model` is fully supported.

<oc-text-input label="Text" v-model="inputValue"/>
<oc-text-input disabled label="Text" v-model="inputValue"/>

### Interactions

<oc-button @click="focus" class="oc-my-m">Focus input below</oc-button>
<oc-text-input label="Focus field" ref="inputForFocusRef"/>
<oc-button @click="focusAndSelect" class="oc-my-m">Focus and select input below</oc-button>
<oc-text-input label="Select field" value="Will you select this existing text?" ref="inputForFocusSelectRef"/>
<oc-text-input label="Clear input" v-model="inputValueForClearing" :clear-button-enabled="true"/>
<oc-text-input label="Input with default" v-model="inputValueWithDefault" :clear-button-enabled="true"
                default-value="Some default"/>

<p>
    Value: {{ inputValueWithDefault !== null ? inputValueWithDefault : "null" }}
</p>

### Messages

<oc-text-input
    label="Input with description message below"
    class="oc-mb-s"
    description-message="This is a description message."
    :fix-message-line="true"
/>
<oc-text-input
    label="Input with error and warning messages with reserved space below"
    class="oc-mb-s"
    v-model="valueForMessages"
    :error-message="errorMessage"
    :warning-message="warningMessage"
    :fix-message-line="true"
/>
<oc-text-input
    label="Input with error and warning messages without reserved space below"
    class="oc-mb-s"
    v-model="valueForMessages"
    :error-message="errorMessage"
    :warning-message="warningMessage"
/>

<script setup lang="ts">
import { computed, ref, unref } from 'vue'

const name = ref<string>('')

const inputValue = ref<string>('initial')
const valueForMessages = ref<string>('')
const inputValueForClearing = ref<string>('clear me')
const inputValueWithDefault = ref<string | null>(null)

const errorMessage = computed(() => {
    return unref(valueForMessages).length === 0 ? 'Value is required.' : ''
})
const warningMessage = computed(() => {
    return unref(valueForMessages).endsWith(' ') ? 'Trailing whitespace should be avoided.' : ''
})

const inputForFocusRef = ref(null)
const focus = () => {
    unref(inputForFocusRef).focus()
}
const inputForFocusSelectRef = ref(null)
const focusAndSelect = () => {
    unref(inputForFocusSelectRef).focus()
}
</script>
