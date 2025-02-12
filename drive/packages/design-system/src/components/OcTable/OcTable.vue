<template>
  <table v-bind="extractTableProps()" class="has-item-context-menu">
    <oc-thead v-if="hasHeader">
      <oc-tr class="oc-table-header-row">
        <oc-th
          v-for="(field, index) in fields"
          :key="`oc-thead-${field.name}`"
          v-bind="extractThProps(field, index)"
        >
          <oc-button
            v-if="field.sortable"
            :aria-label="getSortLabel(field.name)"
            appearance="raw"
            class="oc-button-sort oc-width-1-1"
            @click="handleSort(field)"
          >
            <span v-if="field.headerType === 'slot'" class="oc-table-thead-content">
              <slot :name="field.name + 'Header'" />
            </span>
            <span
              v-else
              class="oc-table-thead-content header-text"
              v-text="extractFieldTitle(field)"
            />
            <oc-icon
              :name="sortDir === 'asc' ? 'arrow-down' : 'arrow-up'"
              fill-type="line"
              :class="{ 'oc-invisible-sr': sortBy !== field.name }"
              size="small"
              variation="passive"
            />
          </oc-button>
          <div v-else>
            <span v-if="field.headerType === 'slot'" class="oc-table-thead-content">
              <slot :name="field.name + 'Header'" />
            </span>
            <span
              v-else
              class="oc-table-thead-content header-text"
              v-text="extractFieldTitle(field)"
            />
          </div>
        </oc-th>
      </oc-tr>
    </oc-thead>
    <oc-tbody class="has-item-context-menu">
      <oc-tr
        v-for="(item, trIndex) in data"
        :key="`oc-tbody-tr-${itemDomSelector(item) || trIndex}`"
        :ref="`row-${trIndex}`"
        v-bind="extractTbodyTrProps(item, trIndex)"
        :data-item-id="item[idKey as keyof Item]"
        :draggable="dragDrop"
        @click="$emit(constants.EVENT_TROW_CLICKED, [item, $event])"
        @contextmenu="
          $emit(
            constants.EVENT_TROW_CONTEXTMENU,
            ($refs[`row-${trIndex}`] as HTMLElement[])[0],
            $event,
            item
          )
        "
        @vue:mounted="
          $emit(constants.EVENT_TROW_MOUNTED, item, ($refs[`row-${trIndex}`] as HTMLElement[])[0])
        "
        @dragstart="dragStart(item, $event)"
        @drop="dropRowEvent(itemDomSelector(item), $event)"
        @dragenter.prevent="dropRowStyling(itemDomSelector(item), false, $event)"
        @dragleave.prevent="dropRowStyling(itemDomSelector(item), true, $event)"
        @mouseleave="dropRowStyling(itemDomSelector(item), true, $event)"
        @dragover="dragOver($event)"
        @item-visible="$emit('itemVisible', item)"
      >
        <oc-td
          v-for="(field, tdIndex) in fields"
          :key="'oc-tbody-td-' + cellKey(field, tdIndex, item)"
          v-bind="extractTdProps(field, tdIndex, item)"
        >
          <slot v-if="isFieldTypeSlot(field)" :name="field.name" :item="item" />
          <template v-else-if="isFieldTypeCallback(field)">
            {{ field.callback(item[field.name as keyof Item]) }}
          </template>
          <template v-else>
            {{ item[field.name as keyof Item] }}
          </template>
        </oc-td>
      </oc-tr>
    </oc-tbody>
    <tfoot v-if="$slots.footer" class="oc-table-footer">
      <tr class="oc-table-footer-row">
        <td :colspan="fullColspan" class="oc-table-footer-cell">
          <!-- @slot Footer of the table -->
          <slot name="footer" />
        </td>
      </tr>
    </tfoot>
  </table>
</template>
<script lang="ts">
import OcThead from '../OcTableHead/OcTableHead.vue'
import OcTbody from '../OcTableBody/OcTableBody.vue'
import OcTr from '../OcTableTr/OcTableTr.vue'
import OcTh from '../OcTableTh/OcTableTh.vue'
import OcTd from '../OcTableTd/OcTableTd.vue'
import OcButton from '../OcButton/OcButton.vue'
import { getSizeClass, Item, FieldType } from '../../helpers'
import { defineComponent, PropType } from 'vue'

import {
  EVENT_THEAD_CLICKED,
  EVENT_TROW_CLICKED,
  EVENT_TROW_MOUNTED,
  EVENT_TROW_CONTEXTMENU,
  EVENT_ITEM_DROPPED,
  EVENT_ITEM_DRAGGED,
  EVENT_SORT
} from '../../helpers/constants'

const SORT_DIRECTION_ASC = 'asc' as const
const SORT_DIRECTION_DESC = 'desc' as const

/**
 * A table component with dynamic layout and data.
 */
export default defineComponent({
  name: 'OcTable',
  status: 'ready',
  release: '2.1.0',
  components: {
    OcThead,
    OcTbody,
    OcTr,
    OcTh,
    OcTd,
    OcButton
  },
  props: {
    /**
     * The data for the table. Each array item will be rendered as one table row. Each array item needs to have a
     * unique identifier. By default we expect this to be an `id` field. If your field has a different name, please
     * specify it in the `id-key` property of oc-table.
     */
    data: {
      type: Array as PropType<Item[]>,
      required: true
    },
    /**
     * Name of the id property of your data items. See `data` for details on how to use it. The [idKey] is a required field
     * within your data items if you want to have working highlighting. For data representation it is not needed.
     */
    idKey: {
      type: String,
      default: 'id'
    },
    /**
     * Closure function to mutate the item id into a valid DOM selector
     */
    itemDomSelector: {
      type: Function,
      required: false,
      default(item: Item) {
        return item[(this as any).idKey as keyof Item]
      }
    },
    /**
     * The column layout of the table.
     *
     * Each field can have the following data:<br />
     * - **name**: values need to be keys of your data items. Required.<br />
     * - **title**: title as displayed in the table header. Optional, falls back to the value of name.<br />
     * - **headerType**: the header field type, can be `slot`, entirely absent or unknown. If absent or unknown, the data will be rendered into a plain table cell.<br />
     * - **type**: the field type, can be `slot`, `callback`, entirely absent or unknown. If absent or unknown, the data will be rendered into a plain table cell.<br />
     * - **callback**: if `type="callback"` the return value of field.callback will be rendered into a plain table cell.<br />
     * - **alignH**: horizontal cell content alignment, can be `left`, `center` or `right`. Defaults to `left`.<br />
     * - **alignV**: vertical cell content alignment, can be `top`, `middle` or `bottom`. Defaults to `middle`.<br />
     * - **width**: horizontal size of a cell, can be `auto`, `shrink` or `expand`. Defaults to `auto`.<br />
     * - **wrap**: text behaviour of a data cell, can be `truncate`, `overflow`, `nowrap`, `break`. Omitted if not set. Header cells are always fixed to `nowrap`.<br />
     * - **thClass**:additional classes on header cells, provided as a string, classes separated by spaces. Optional, falls back to an empty string.<br />
     * - **tdClass**: additional classes on data cells, provided as a string, classes separated by spaces. Optional, falls back to an empty string.<br />
     * - **sortable**: defines if the column is sortable, can be `true` or `false`.
     */
    fields: {
      type: Array as PropType<FieldType[]>,
      required: true
    },
    /**
     * Asserts whether the table has a header. The header markup is defined in the `fields` array.
     */
    hasHeader: {
      type: Boolean,
      default: true
    },
    /**
     * Asserts whether the header of the table is sticky.
     */
    sticky: {
      type: Boolean,
      required: false,
      default: false
    },
    /**
     * Asserts whether table rows should be highlighted when hovered.
     */
    hover: {
      type: Boolean,
      default: false
    },
    /**
     * The ids of highlighted data items. Null or an empty string/array for no highlighting.
     */
    highlighted: {
      type: [String, Array],
      default: null
    },
    /**
     * The ids of disabled data items. Empty array for no disabled items.
     */
    disabled: {
      type: Array as PropType<Array<string | number>>,
      default: (): Array<string | number> => []
    },
    /**
     * Top position of header used when the header is sticky in pixels
     */
    headerPosition: {
      type: Number,
      required: false,
      default: 0
    },
    /**
     * Sets the padding size for x axis
     * @values xsmall, small, medium, large, xlarge
     */
    paddingX: {
      type: String,
      required: false,
      default: 'small',
      validator: (size: string) => ['xsmall', 'small', 'medium', 'large', 'xlarge'].includes(size)
    },
    /**
     * Enable Drag & Drop events
     */
    dragDrop: {
      type: Boolean,
      required: false,
      default: false
    },
    /**
     * Array of items that should be selected by default.
     */
    selection: {
      type: Array as PropType<Item[]>,
      required: false,
      default: (): Item[] => []
    },
    /**
     * Determines if the table content should be loaded lazily.
     */
    lazy: {
      type: Boolean,
      default: false
    },
    /**
     * Show that the table is sorted ascendingly/descendingly (no actual sorting takes place)
     */
    sortDir: {
      type: String,
      required: false,
      default: undefined,
      validator: (value: string) => {
        return value === undefined || ['asc', 'desc'].includes(value)
      }
    },
    /**
     * Show that the table is sorted by this column (no actual sorting takes place)
     */
    sortBy: {
      type: String,
      required: false,
      default: undefined
    },
    /**
     * This is only relevant for CERN and can be ignored in any other cases.
     */
    groupingSettings: {
      type: Object,
      required: false,
      default: null
    }
  },
  emits: [
    EVENT_ITEM_DROPPED,
    EVENT_ITEM_DRAGGED,
    EVENT_THEAD_CLICKED,
    EVENT_TROW_CLICKED,
    EVENT_TROW_MOUNTED,
    EVENT_TROW_CONTEXTMENU,
    EVENT_SORT,
    'dropRowStyling',
    'itemVisible'
  ],
  setup() {
    const constants = {
      EVENT_THEAD_CLICKED,
      EVENT_TROW_CLICKED,
      EVENT_TROW_MOUNTED,
      EVENT_TROW_CONTEXTMENU
    }
    return { constants }
  },
  computed: {
    isSortable() {
      return this.fields.some((f) => f.sortable)
    },
    tableClasses() {
      const result = ['oc-table']

      if (this.hover) {
        result.push('oc-table-hover')
      }

      if (this.sticky) {
        result.push('oc-table-sticky')
      }

      return result
    },

    fullColspan() {
      return this.fields.length
    }
  },
  methods: {
    dragOver(event: DragEvent) {
      event.preventDefault()
    },
    dragStart(item: Item, event: DragEvent) {
      this.$emit(EVENT_ITEM_DRAGGED, item, event)
    },
    dropRowEvent(selector: Item, event: DragEvent) {
      this.$emit(EVENT_ITEM_DROPPED, selector, event)
    },
    dropRowStyling(selector: Item, leaving: boolean, event: DragEvent) {
      this.$emit('dropRowStyling', selector, leaving, event)
    },
    isFieldTypeSlot(field: FieldType) {
      return field.type === 'slot'
    },
    isFieldTypeCallback(field: FieldType) {
      return ['callback', 'function'].indexOf(field.type) >= 0
    },
    extractFieldTitle(field: FieldType) {
      if (Object.prototype.hasOwnProperty.call(field, 'title')) {
        return field.title
      }
      return field.name
    },
    extractTableProps() {
      return {
        class: this.tableClasses
      }
    },
    extractThProps(field: FieldType, index: number) {
      const props = this.extractCellProps(field)
      props.class = `oc-table-header-cell oc-table-header-cell-${field.name}`
      if (Object.prototype.hasOwnProperty.call(field, 'thClass')) {
        props.class += ` ${field.thClass}`
      }
      if (this.sticky) {
        props.style = `top: ${this.headerPosition}px;`
      }

      if (index === 0) {
        props.class += ` oc-pl-${getSizeClass(this.paddingX)} `
      }

      if (index === this.fields.length - 1) {
        props.class += ` oc-pr-${getSizeClass(this.paddingX)}`
      }

      this.extractSortThProps(props, field)

      return props
    },
    extractTbodyTrProps(item: Item, index: number) {
      return {
        ...(this.lazy && { lazy: { colspan: this.fullColspan } }),
        class: [
          'oc-tbody-tr',
          `oc-tbody-tr-${this.itemDomSelector(item) || index}`,
          this.isHighlighted(item) ? 'oc-table-highlighted' : undefined,
          this.isDisabled(item) ? 'oc-table-disabled' : undefined
        ].filter(Boolean)
      }
    },
    extractTdProps(field: FieldType, index: number, item: Item) {
      const props = this.extractCellProps(field)
      props.class = `oc-table-data-cell oc-table-data-cell-${field.name}`
      if (Object.prototype.hasOwnProperty.call(field, 'tdClass')) {
        props.class += ` ${field.tdClass}`
      }
      if (Object.prototype.hasOwnProperty.call(field, 'wrap')) {
        props.wrap = field.wrap
      }

      if (index === 0) {
        props.class += ` oc-pl-${getSizeClass(this.paddingX)} `
      }

      if (index === this.fields.length - 1) {
        props.class += ` oc-pr-${getSizeClass(this.paddingX)}`
      }

      if (Object.prototype.hasOwnProperty.call(field, 'accessibleLabelCallback')) {
        props['aria-label'] = field.accessibleLabelCallback(item)
      }

      return props
    },
    extractCellProps(field: FieldType): Record<string, string> {
      return {
        ...(field?.alignH && { alignH: field.alignH }),
        ...(field?.alignV && { alignV: field.alignV }),
        ...(field?.width && { width: field.width }),
        class: undefined,
        wrap: undefined,
        style: undefined
      }
    },
    isHighlighted(item: Item) {
      if (!this.highlighted) {
        return false
      }

      if (Array.isArray(this.highlighted)) {
        return this.highlighted.indexOf(item[this.idKey as keyof Item]) > -1
      }

      return this.highlighted === item[this.idKey as keyof Item]
    },
    isDisabled(item: Item) {
      if (!this.disabled.length) {
        return false
      }

      return this.disabled.indexOf(item[this.idKey as keyof Item]) > -1
    },

    cellKey(field: FieldType, index: number, item: Item) {
      const prefix = [item[this.idKey as keyof Item], index + 1].filter(Boolean)

      if (this.isFieldTypeSlot(field)) {
        return [...prefix, field.name].join('-')
      }

      if (this.isFieldTypeCallback(field)) {
        return [...prefix, field.callback(item[field.name as keyof Item])].join('-')
      }

      return [...prefix, item[field.name as keyof Item]].join('-')
    },

    getSortLabel(name: string) {
      return this.$gettext('Sort by %{ name }', { name })
    },

    extractSortThProps(props: Record<string, string>, field: FieldType) {
      if (!this.fieldIsSortable(field)) {
        return
      }

      let sort = 'none'
      if (this.sortBy === field.name) {
        sort = this.sortDir === SORT_DIRECTION_ASC ? 'ascending' : 'descending'
      }
      props['aria-sort'] = sort
    },
    fieldIsSortable({ sortable }: FieldType) {
      return !!sortable
    },
    handleSort(field: FieldType) {
      if (!this.fieldIsSortable(field)) {
        return
      }

      let sortDir = this.sortDir
      // toggle sortDir if already sorted by this column
      if (this.sortBy === field.name && this.sortDir !== undefined) {
        sortDir = this.sortDir === SORT_DIRECTION_DESC ? SORT_DIRECTION_ASC : SORT_DIRECTION_DESC
      }
      // set default sortDir of the field when sortDir not set or sortBy changed
      if (this.sortBy !== field.name || this.sortDir === undefined) {
        sortDir = field.sortDir || SORT_DIRECTION_DESC
      }

      /**
       * Triggers when table heads are clicked
       *
       * @property {string} sortBy requested column to sort by
       * @property {string} sortDir requested order to sort in (either asc or desc)
       */
      this.$emit('sort', {
        sortBy: field.name,
        sortDir
      })
    }
  }
})
</script>
<style lang="scss">
.oc-table {
  border-collapse: collapse;
  border-spacing: 0;
  color: var(--oc-color-text-default);
  width: 100%;

  &-hover tr {
    transition: background-color $transition-duration-short ease-in-out;
  }

  tr {
    outline: none;
    height: var(--oc-size-height-table-row);
  }

  tr + tr {
    border-top: 1px solid var(--oc-color-border);
  }

  &-hover tr:not(&-footer-row):hover {
    background-color: var(--oc-color-background-hover);
  }

  &-hover
    tr:hover
    td:not(:last-child)
    span:not(.avatarInitials):not(button span):not(.oc-table-highlighted span) {
    color: var(--oc-color-swatch-brand-contrast) !important;
  }

  &-highlighted {
    background-color: var(--oc-color-background-highlight) !important;
  }

  &-accentuated {
    background-color: var(--oc-color-background-accentuate);
  }

  &-disabled {
    background-color: var(--oc-color-background-muted);
    opacity: 0.7;
    filter: grayscale(0.6);
    pointer-events: none;
  }

  &-sticky {
    position: relative;

    .oc-table-header-cell {
      background-color: var(--oc-color-background-default);
      position: sticky;
      z-index: 1;
    }
  }

  .highlightedDropTarget {
    background-color: var(--oc-color-input-border);
  }

  &-thead-content {
    vertical-align: middle;
    display: inline-table;
    color: var(--oc-color-swatch-passive-default);
    &:hover {
      text-decoration: underline;
    }
  }

  &-footer {
    border-top: 1px solid var(--oc-color-border);

    &-cell {
      color: var(--oc-color-text-muted);
      font-size: 0.875rem;
      line-height: 1.4;
      padding: var(--oc-space-xsmall);
    }
  }
}
.oc-button-sort {
  display: flex;
  justify-content: start;
  .oc-icon {
    &:hover {
      background-color: var(--oc-color-background-hover);
    }
  }
}
</style>
